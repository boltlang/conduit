//! Runs the programs in `tests/programs` that have a `bench` check as Wasm,
//! in Wasmtime, and measures each of their `run` checks, compiled each of
//! the ways the `bench` snapshots compare (see `tests/bench.rs` in
//! `conduit-opt`):
//!
//! - **fuel**: what Wasmtime counts as it runs the code, about one per Wasm
//!   instruction. It's the same every time, so it can be compared exactly.
//! - **time**: the median wall-clock time of the call, with how far the
//!   runs spread around it, over enough runs to fill `--time`. The module
//!   is compiled once, and each run gets a fresh instance, outside the
//!   timing. Times are only worth comparing on a quiet machine, with
//!   `--release`.
//! - **code**: the size of the module, in bytes, and **pages**: how much
//!   memory it has after the call, in 64 KiB pages.
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
//! - `--fuel`: no times, which is quick and the same every time.
//! - `--time MS`: how long to time each call for, 300 by default.
//! - `--json FILE`: also write the results to `FILE`, with the commit.
//! - `--compare FILE`: compare with results `--json` wrote, and show what
//!   changed: any change in fuel, and times more than 5% and more than
//!   their spread apart.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use conduit_ir::{Atom, FuncId, Lit, Module, Ty};
use conduit_opt::ownership::{CONFIGS, Config};
use conduit_test::{CheckKind, Outcome, Program};
use conduit_wasm::Options;
use serde_json::{Value as Json, json};
use wasmtime::{Engine, Linker, Store, Val};

struct Args {
    filter: Option<String>,
    configs: Vec<String>,
    fuel_only: bool,
    time: Duration,
    json: Option<PathBuf>,
    compare: Option<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        filter: None,
        configs: CONFIGS.iter().map(|c| c.name.to_string()).collect(),
        fuel_only: false,
        time: Duration::from_millis(300),
        json: None,
        compare: None,
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
            "--fuel" => args.fuel_only = true,
            "--time" => {
                let ms = value("--time")?
                    .parse()
                    .map_err(|e| format!("--time: {e}"))?;
                args.time = Duration::from_millis(ms);
            }
            "--json" => args.json = Some(value("--json")?.into()),
            "--compare" => args.compare = Some(value("--compare")?.into()),
            "--help" | "-h" => {
                return Err(
                    "usage: conduit-bench [--config NAME,..] [--fuel] [--time MS] \
                            [--json FILE] [--compare FILE] [filter]"
                        .into(),
                );
            }
            _ if a.starts_with("--") => return Err(format!("unknown option {a}")),
            _ => args.filter = Some(a),
        }
    }
    Ok(args)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// One measurement, of one `run` check compiled one way.
struct Result1 {
    program: String,
    run: String,
    config: String,
    fuel: u64,
    /// The median and the median distance from it, if timed.
    time: Option<(Duration, Duration)>,
    code: usize,
    pages: u64,
}

fn run() -> Result<(), String> {
    let args = args()?;
    let baseline = match &args.compare {
        Some(path) => Some(load(path)?),
        None => None,
    };
    let mut results = Vec::new();
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
    }
    print(&results, baseline.as_ref());
    if let Some(path) = &args.json {
        let json = json!({
            "commit": commit(),
            "results": results.iter().map(|r| json!({
                "program": r.program,
                "run": r.run,
                "config": r.config,
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
    Ok(())
}

/// The modules of `p`, one for each config asked for, with the options to
/// lower it with.
fn modules(p: &Program, configs: &[String]) -> Result<Vec<(String, Module, Options)>, String> {
    let mut optimized = p.module()?;
    if !conduit_ir::clause::direct_handlers(&optimized) {
        conduit_opt::effects::lower_effects(&mut optimized)?;
    }
    conduit_opt::optimize_module(&mut optimized);
    let mut out = Vec::new();
    for name in configs {
        let config = Config::named(name).ok_or_else(|| format!("unknown config {name}"))?;
        let mut m = optimized.clone();
        config.apply(&mut m)?;
        let options = Options {
            stack_switching: false,
            counts: config.counts,
            cycles: config.cycles,
        };
        out.push((name.clone(), m, options));
    }
    Ok(out)
}

fn bench(p: &Program, args: &Args) -> Result<Vec<Result1>, String> {
    let fuel_engine = engine(true)?;
    let time_engine = engine(false)?;
    // Each config compiled, once for fuel and once for timing.
    let mut compiled = Vec::new();
    for (config, m, options) in modules(p, &args.configs)? {
        let runs = p.runs(&m)?;
        let funcs: std::collections::BTreeSet<FuncId> = runs.iter().map(|r| r.func).collect();
        let names: Vec<(FuncId, String)> = funcs.iter().map(|&f| (f, f.to_string())).collect();
        let exports: Vec<(FuncId, &str)> = names.iter().map(|(f, n)| (*f, n.as_str())).collect();
        let bytes = conduit_wasm::emit_module_with(&m, &exports, &options)
            .map_err(|e| format!("{config}: {e}"))?;
        let fuel_module =
            wasmtime::Module::new(&fuel_engine, &bytes).map_err(|e| format!("{config}: {e:?}"))?;
        let time_module = (!args.fuel_only)
            .then(|| wasmtime::Module::new(&time_engine, &bytes))
            .transpose()
            .map_err(|e| format!("{config}: {e:?}"))?;
        compiled.push((config, m, bytes.len(), fuel_module, time_module));
    }
    let Some((_, first, ..)) = compiled.first() else {
        return Ok(Vec::new());
    };
    // Passes keep functions' IDs, so the runs are the same in every config.
    let runs = p.runs(first)?;
    let mut out = Vec::new();
    for run in &runs {
        let args_text: Vec<String> = run.args.iter().map(|&l| Atom::Lit(l).to_string()).collect();
        let call = format!("{}({})", run.name, args_text.join(", "));
        let results = first
            .funcs
            .get(&run.func)
            .map_or(&[][..], |f| &f.results[..]);
        // Fuel, pages and the results, in one run of each.
        for (config, _, code, fuel_module, _) in &compiled {
            let mut store = Store::new(&fuel_engine, ());
            store.set_fuel(u64::MAX / 2).map_err(|e| e.to_string())?;
            let instance = instantiate(&fuel_engine, &mut store, fuel_module)?;
            let got = call_func(&mut store, &instance, run.func, run.args, results);
            let fuel = u64::MAX / 2 - store.get_fuel().map_err(|e| e.to_string())?;
            let pages = instance
                .get_memory(&mut store, "memory")
                .map_or(0, |mem| mem.size(&store));
            run.check(p, got).map_err(|e| format!("{config}: {e}"))?;
            out.push(Result1 {
                program: p.name.clone(),
                run: call.clone(),
                config: config.clone(),
                fuel,
                time: None,
                code: *code,
                pages,
            });
        }
        if args.fuel_only {
            continue;
        }
        let modules: Vec<&wasmtime::Module> = (compiled.iter())
            .filter_map(|(.., time_module)| time_module.as_ref())
            .collect();
        let times = time_calls(&time_engine, &modules, run, results, args.time)?;
        let n = out.len() - times.len();
        for (r, t) in out[n..].iter_mut().zip(times) {
            r.time = Some(t);
        }
    }
    Ok(out)
}

/// For each of `modules`, the median time of calling `run`, and the median
/// distance from it. The modules take turns, a call each per round, so
/// that the machine changing speed over time changes them all alike, over
/// at least 5 rounds and as many as fill `budget` for each. A call of each
/// first warms it up.
fn time_calls(
    engine: &Engine,
    modules: &[&wasmtime::Module],
    run: &conduit_test::Run<'_>,
    results: &[Ty],
    budget: Duration,
) -> Result<Vec<(Duration, Duration)>, String> {
    let once = |module: &wasmtime::Module| -> Result<Duration, String> {
        let mut store = Store::new(engine, ());
        let instance = instantiate(engine, &mut store, module)?;
        let t = Instant::now();
        let got = call_func(&mut store, &instance, run.func, run.args, results);
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
    Ok(times
        .into_iter()
        .map(|mut ts| {
            ts.sort();
            let median = ts[ts.len() / 2];
            let mut spread: Vec<Duration> = ts.iter().map(|&t| t.abs_diff(median)).collect();
            spread.sort();
            (median, spread[spread.len() / 2])
        })
        .collect())
}

fn engine(fuel: bool) -> Result<Engine, String> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_tail_call(true)
        .wasm_exceptions(true)
        .consume_fuel(fuel)
        // Instantiating from a pool is quicker, which leaves more of the
        // time for runs.
        .allocation_strategy(wasmtime::InstanceAllocationStrategy::pooling());
    Engine::new(&config).map_err(|e| e.to_string())
}

fn instantiate(
    engine: &Engine,
    store: &mut Store<()>,
    module: &wasmtime::Module,
) -> Result<wasmtime::Instance, String> {
    let mut linker = Linker::new(engine);
    // The same host functions as the test programs get.
    linker
        .func_wrap("test", "double", |n: i32| n.wrapping_mul(2))
        .map_err(|e| e.to_string())?;
    linker.instantiate(store, module).map_err(|e| e.to_string())
}

fn call_func(
    store: &mut Store<()>,
    instance: &wasmtime::Instance,
    func: FuncId,
    args: &[Lit],
    result_tys: &[Ty],
) -> Result<Outcome, String> {
    let name = func.to_string();
    let f = instance
        .get_func(&mut *store, &name)
        .ok_or_else(|| format!("{name} isn't exported"))?;
    let params: Vec<Val> = args
        .iter()
        .map(|l| match *l {
            Lit::I8(n) => Val::I32(n.into()),
            Lit::I16(n) => Val::I32(n.into()),
            Lit::I32(n) => Val::I32(n),
            Lit::I64(n) => Val::I64(n),
            Lit::F32(bits) => Val::F32(bits),
            Lit::F64(bits) => Val::F64(bits),
        })
        .collect();
    let mut results = vec![Val::I32(0); f.ty(&*store).results().len()];
    match f.call(&mut *store, &params, &mut results) {
        Ok(()) => results
            .into_iter()
            .zip(result_tys.iter().map(Some).chain(std::iter::repeat(None)))
            .map(|(v, ty)| match (v, ty) {
                (Val::I32(n), Some(Ty::I8)) => Ok(Lit::I8(n as i8)),
                (Val::I32(n), Some(Ty::I16)) => Ok(Lit::I16(n as i16)),
                (Val::I32(n), _) => Ok(Lit::I32(n)),
                (Val::I64(n), _) => Ok(Lit::I64(n)),
                (Val::F32(bits), _) => Ok(Lit::F32(bits)),
                (Val::F64(bits), _) => Ok(Lit::F64(bits)),
                _ => Err("gave a reference".to_string()),
            })
            .collect::<Result<_, _>>()
            .map(Outcome::Values),
        Err(e) if e.downcast_ref::<wasmtime::Trap>().is_some() => Ok(Outcome::Trap(e.to_string())),
        Err(e) => Err(e.to_string()),
    }
}

/// A baseline's measurements, by program, run and config: fuel, and the
/// time and spread in nanoseconds, if timed.
type Baseline = BTreeMap<(String, String, String), (u64, Option<(u64, u64)>)>;

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
        out.insert(
            (s("program"), s("run"), s("config")),
            (r["fuel"].as_u64().unwrap_or(0), time),
        );
    }
    Ok(out)
}

fn print(results: &[Result1], baseline: Option<&Baseline>) {
    let mut last = (String::new(), String::new());
    for r in results {
        if (&r.program, &r.run) != (&last.0, &last.1) {
            println!("\n{} {}", r.program, r.run);
            println!(
                "  {:<14}{:>14}{:>12}{:>10}{:>9}{:>7}",
                "", "fuel", "time", "±", "code", "pages"
            );
            last = (r.program.clone(), r.run.clone());
        }
        let (time, spread) = match r.time {
            Some((t, s)) => (show(t), show(s)),
            None => ("-".into(), "-".into()),
        };
        let mut line = format!(
            "  {:<14}{:>14}{:>12}{:>10}{:>9}{:>7}",
            r.config, r.fuel, time, spread, r.code, r.pages
        );
        let key = (r.program.clone(), r.run.clone(), r.config.clone());
        if let Some(&(fuel, base_time)) = baseline.and_then(|b| b.get(&key)) {
            if fuel != r.fuel {
                line.push_str(&format!("  fuel {}", change(fuel as f64, r.fuel as f64)));
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
