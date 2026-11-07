//! Runs the programs in `tests/programs` that have a `bench` check as Wasm,
//! in Wasmtime, and measures each of their `run` checks, compiled each of
//! the ways the `bench` snapshots compare ([`conduit_bench::CONFIGS`]):
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
//! - `--markdown`: print a Markdown table instead, of only the fuel that
//!   changed with `--compare`, as CI shows in a pull request's summary.
//! - `--check PCT`: with `--compare`, fail if any fuel went up by more
//!   than `PCT` percent, which CI does for a pull request.
//! - `--badges DIR`: write badges for the README to `DIR`, in the JSON
//!   that shields.io's endpoint badges read, which CI publishes for each
//!   push to `main`. They summarize the benchmarks in `bench/`: what
//!   counting costs in fuel, with everything on, over no counts, how many
//!   fewer objects reuse allocates, and how many benchmarks there are.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use conduit_bench::{CONFIGS, Config, prepare};
use conduit_ir::{Atom, FuncId, Module, Ty};
use conduit_test::{CheckKind, Program};
use conduit_wasm::Options;
use serde_json::{Value as Json, json};
use wasmtime::{Engine, Store};

struct Args {
    filter: Option<String>,
    configs: Vec<String>,
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
                    "usage: conduit-bench [--config NAME,..] [--fuel] [--time MS] \
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
fn fuel_changes<'r>(results: &'r [Result1], baseline: &Baseline) -> Vec<(&'r Result1, u64, u64)> {
    (results.iter())
        .filter_map(|r| {
            let key = (r.program.clone(), r.run.clone(), r.config.clone());
            let &(before, _) = baseline.get(&key)?;
            (before != r.fuel).then_some((r, before, r.fuel))
        })
        .collect()
}

/// The results as a Markdown table: with a baseline, only the fuel that
/// changed, else all of them.
fn markdown(results: &[Result1], baseline: Option<&Baseline>) {
    match baseline {
        Some(baseline) => {
            let changes = fuel_changes(results, baseline);
            println!("### Fuel\n");
            if changes.is_empty() {
                println!("No change in fuel in any benchmark.");
                return;
            }
            println!("| Program | Run | Config | Before | After | Change |");
            println!("|---|---|---|---:|---:|---:|");
            for (r, before, after) in changes {
                println!(
                    "| `{}` | `{}` | `{}` | {before} | {after} | {} |",
                    r.program,
                    r.run,
                    r.config,
                    change(before as f64, after as f64)
                );
            }
        }
        None => {
            println!("| Program | Run | Config | Fuel | Time | Code | Pages |");
            println!("|---|---|---|---:|---:|---:|---:|");
            for r in results {
                let time = r.time.map_or("-".into(), |(t, _)| show(t));
                println!(
                    "| `{}` | `{}` | `{}` | {} | {time} | {} | {} |",
                    r.program, r.run, r.config, r.fuel, r.code, r.pages
                );
            }
        }
    }
}

/// The modules of `p`, one for each config asked for, with the options to
/// lower it with.
fn modules(p: &Program, configs: &[String]) -> Result<Vec<(String, Module, Options)>, String> {
    let optimized = prepare(p.module()?)?;
    let mut out = Vec::new();
    for name in configs {
        let config = Config::named(name).ok_or_else(|| format!("unknown config {name}"))?;
        let mut m = optimized.clone();
        config.apply(&mut m)?;
        let options = Options {
            stack_switching: false,
            counts: config.counts,
            cycles: config.cycles,
            ..Default::default()
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
        let mut m = m;
        conduit_test::link_for_wasm(&mut m);
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
            let got = conduit_test::wasm::call(&mut store, &instance, run.func, run.args, results);
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
        let got = conduit_test::wasm::call(&mut store, &instance, run.func, run.args, results);
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
    let mut config = conduit_test::wasm::config();
    config
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
    let linker = conduit_test::wasm::linker(engine)?;
    linker.instantiate(store, module).map_err(|e| e.to_string())
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

/// The programs the badges summarize: the benchmarks in the style of the
/// Perceus paper's, not the test programs that also have a `bench` check.
const BADGE_PROGRAMS: &str = "bench/";

/// Writes the README's badges to `dir`, as shields.io endpoint JSON:
/// `overhead.json`, the geometric mean over the benchmarks' runs of the
/// fuel with everything on (`+cycles`) over the fuel without counts
/// (`optimized`); `reuse.json`, how many fewer objects the evaluator
/// allocates with reuse than with counts alone, over all the runs; and
/// `benchmarks.json`, how many there are.
fn badges(results: &[Result1], programs: &[Program], dir: &Path) -> Result<(), String> {
    let fuel = |program: &str, run: &str, config: &str| {
        (results.iter())
            .find(|r| r.program == program && r.run == run && r.config == config)
            .map(|r| r.fuel as f64)
    };
    let mut logs = Vec::new();
    for r in results
        .iter()
        .filter(|r| r.program.starts_with(BADGE_PROGRAMS))
    {
        if r.config != "optimized" {
            continue;
        }
        let counted = fuel(&r.program, &r.run, "+cycles")
            .ok_or("the badges need the optimized and +cycles configs")?;
        logs.push((counted / r.fuel as f64).ln());
    }
    if logs.is_empty() {
        return Err(format!(
            "no benchmarks in {BADGE_PROGRAMS} to make badges of"
        ));
    }
    let overhead = (logs.iter().sum::<f64>() / logs.len() as f64).exp() - 1.0;

    // Allocations, in the evaluator, which counts them.
    let (mut without, mut with) = (0u64, 0u64);
    let mut count = 0;
    for p in programs
        .iter()
        .filter(|p| p.name.starts_with(BADGE_PROGRAMS))
    {
        count += 1;
        let base = prepare(p.module()?)?;
        let mut compiled = Vec::new();
        for name in ["counts", "+reuse"] {
            let mut m = base.clone();
            Config::named(name).unwrap().apply(&mut m)?;
            compiled.push(m);
        }
        for run in p.runs(&base)? {
            let allocs = |m: &Module| {
                let mut machine = conduit_eval::Machine::new(m, conduit_test::eval::TestHost);
                machine.counting = true;
                let args = run
                    .args
                    .iter()
                    .map(|&l| conduit_eval::Value::from(l))
                    .collect();
                machine.call(run.func, args).map_err(|e| format!("{e:?}"))?;
                Ok::<_, String>(machine.stats.allocs)
            };
            without += allocs(&compiled[0])?;
            with += allocs(&compiled[1])?;
        }
    }
    let saved = if without == 0 {
        0.0
    } else {
        1.0 - with as f64 / without as f64
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
