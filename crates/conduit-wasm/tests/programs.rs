//! Lowers every test program to Wasm, validates it without the GC and
//! stack switching proposals, compares it with its `<name>.wat` snapshot
//! if it has a `wat` check, and runs its `run` checks with Wasmtime.
//! Programs with `wasm-error` checks must fail to lower with those
//! messages instead.
//!
//! A program with a handler whose clauses aren't all tail-resumptive or
//! aborting is lowered by evidence passing by default, and is lowered and
//! run again with stack switching, which is then allowed.
//!
//! Each program is also lowered with its reference counts inserted and
//! freeing on: its effects lowered by evidence passing first if it needs
//! that, then given reuse, counted and its drops specialized. Each run must then leave
//! as many objects live as the evaluator's run of the same module does,
//! not counting its continuations, which Wasm doesn't make, both with
//! cycles collected at the end.

mod support;

use conduit_ir::{FuncId, Module};
use conduit_test::{CheckKind, Program};
use conduit_wasm::Options;
use support::{call, instantiate};
use wasmparser::WasmFeatures;

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_) | CheckKind::WasmSkip(_)))
        && (p.has_runs() || p.has(|k| matches!(k, CheckKind::Wat | CheckKind::WasmError(_))))
}

fn test(p: &Program) -> Result<(), String> {
    let m = p.module()?;
    let Some(bytes) = lower(p, &m, false)? else {
        return Ok(());
    };
    if p.has(|k| *k == CheckKind::Wat) {
        let wat = wasmprinter::print_bytes(&bytes).map_err(|e| e.to_string())?;
        p.snapshot("wat", &wat)?;
    }
    run(p, &m, &bytes, false)?;
    if p.has_runs() && !p.has(|k| matches!(k, CheckKind::WasmError(_))) {
        counted(p, &m)?;
    }
    // A general handler is lowered by evidence passing without stack
    // switching, and by stack switching with it.
    if !conduit_ir::clause::direct_handlers(&m) {
        let bytes = lower(p, &m, true)?.expect("lowering failed without stack switching too");
        run(p, &m, &bytes, true)?;
    }
    Ok(())
}

fn with(stack_switching: bool) -> &'static str {
    if stack_switching {
        "with stack switching"
    } else {
        "without stack switching"
    }
}

/// The Wasm of `m`, validated, with each function a `run` check calls
/// exported under its name, or `None` if lowering failed as the `wasm-error` checks expect.
fn lower(p: &Program, m: &Module, stack_switching: bool) -> Result<Option<Vec<u8>>, String> {
    lower_with(
        p,
        m,
        Options {
            stack_switching,
            counts: false,
            cycles: false,
        },
    )
}

fn lower_with(p: &Program, m: &Module, options: Options) -> Result<Option<Vec<u8>>, String> {
    let stack_switching = options.stack_switching;
    let funcs: std::collections::BTreeSet<FuncId> = p.runs(m)?.iter().map(|r| r.func).collect();
    let names: Vec<(FuncId, String)> = funcs.iter().map(|&f| (f, f.to_string())).collect();
    let exports: Vec<(FuncId, &str)> = names.iter().map(|(f, n)| (*f, n.as_str())).collect();
    let expected = p.messages(|k| match k {
        CheckKind::WasmError(m) => Some(m),
        _ => None,
    });
    let bytes = match conduit_wasm::emit_module_with(m, &exports, &options) {
        Ok(_) if !expected.is_empty() => {
            return Err(format!(
                "lowering should fail with {}, but succeeds",
                expected.join("; ")
            ));
        }
        Ok(bytes) => bytes,
        Err(e) if expected.is_empty() => {
            return Err(format!("lowering {} fails: {e}", with(stack_switching)));
        }
        Err(e) => {
            return match expected.iter().find(|m| !e.to_string().contains(*m)) {
                Some(m) => Err(format!(
                    "lowering fails with {e:?}, which doesn't contain {m:?}"
                )),
                None => Ok(None),
            };
        }
    };
    let mut features = WasmFeatures::default() - WasmFeatures::GC - WasmFeatures::STACK_SWITCHING;
    if stack_switching {
        features |= WasmFeatures::STACK_SWITCHING;
    }
    if let Err(e) = wasmparser::Validator::new_with_features(features).validate_all(&bytes) {
        let wat = wasmprinter::print_bytes(&bytes).unwrap_or_default();
        return Err(format!(
            "the Wasm {} doesn't validate without GC: {e}\n{wat}",
            with(stack_switching)
        ));
    }
    Ok(Some(bytes))
}

fn run(p: &Program, m: &Module, bytes: &[u8], stack_switching: bool) -> Result<(), String> {
    if !p.has_runs() {
        return Ok(());
    }
    let (mut store, instance) = instantiate(bytes, stack_switching)?;
    for run in p.runs(m)? {
        let results = m.funcs.get(&run.func).map_or(&[][..], |f| &f.results[..]);
        let got = call(&mut store, &instance, run.func, run.args, results);
        run.check(p, got)
            .map_err(|e| format!("{}: {e}", with(stack_switching)))?;
    }
    Ok(())
}

/// Runs `p` counted and with freeing: see the crate's tests' documentation.
fn counted(p: &Program, m: &Module) -> Result<(), String> {
    let mut c = m.clone();
    if !conduit_ir::clause::direct_handlers(&c) {
        conduit_effects::lower_effects(&mut c)?;
    }
    // A program counted by hand already has its counts.
    if !p.has(|k| matches!(k, CheckKind::Counted)) {
        conduit_opt::ownership::insert_reuse(&mut c);
        conduit_opt::ownership::insert_counts(&mut c)?;
        conduit_opt::ownership::specialize_drops(&mut c);
    }
    let options = Options {
        stack_switching: false,
        counts: true,
        cycles: true,
    };
    let bytes = lower_with(p, &c, options)?.expect("lowering with counts failed");
    let (mut store, instance) = instantiate(&bytes, false)?;
    let live = instance
        .get_global(&mut store, "conduit_live")
        .ok_or("no conduit_live global")?;
    let collect = instance
        .get_typed_func::<(), ()>(&mut store, "conduit_collect")
        .map_err(|e| e.to_string())?;
    for run in p.runs(&c)? {
        let results = c.funcs.get(&run.func).map_or(&[][..], |f| &f.results[..]);
        let before = live.get(&mut store).unwrap_i32();
        let got = call(&mut store, &instance, run.func, run.args, results);
        let gave = matches!(got, Ok(conduit_test::Outcome::Values(_)));
        run.check(p, got).map_err(|e| format!("with counts: {e}"))?;
        if !gave {
            continue;
        }
        collect.call(&mut store, ()).map_err(|e| e.to_string())?;
        let left = live.get(&mut store).unwrap_i32() - before;
        let mut machine = conduit_eval::Machine::new(&c, conduit_test::eval::TestHost);
        machine.counting = true;
        machine.cycles = true;
        let args = run
            .args
            .iter()
            .map(|&l| conduit_eval::Value::from(l))
            .collect();
        machine.call(run.func, args).map_err(|e| e.to_string())?;
        machine.collect_cycles();
        // A continuation is an object in the evaluator, but not in Wasm,
        // which lowers what handlers are left directly.
        let expected = machine
            .objects()
            .filter(|(id, o)| {
                matches!(machine.count(*id), Some(conduit_eval::Count::Live(_)))
                    && !matches!(o, conduit_eval::Object::Cont(_))
            })
            .count() as i32;
        if left != expected {
            return Err(format!(
                "{}: {}: with counts, Wasm leaves {left} objects live, but the evaluator {expected}",
                p.at(run.line),
                run.name
            ));
        }
    }
    Ok(())
}
