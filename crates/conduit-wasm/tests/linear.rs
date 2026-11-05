//! Lowers every test program with `run` checks with the linear memory
//! backend, validates the Wasm without the GC and stack switching
//! proposals, and runs its `run` checks with Wasmtime. A program with a
//! general handler is lowered again with stack switching, which is then
//! allowed.

mod support;

use conduit_ir::FuncId;
use conduit_test::{CheckKind, Program};
use support::{call, instantiate};
use wasmparser::WasmFeatures;

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_))) && p.has_runs()
}

fn test(p: &Program) -> Result<(), String> {
    let m = p.module()?;
    run(p, &m, false)?;
    // A general handler is lowered by evidence passing without stack
    // switching, and by stack switching with it.
    if has_general_handler(&m) {
        run(p, &m, true)?;
    }
    Ok(())
}

fn has_general_handler(m: &conduit_ir::Module) -> bool {
    m.handlers.values().any(|h| {
        h.ops.iter().any(|c| {
            m.funcs.get(c).is_some_and(|f| {
                conduit_ir::clause::plain_call(f).is_none()
                    && conduit_ir::clause::aborting(f).is_none()
            })
        })
    })
}

fn run(p: &Program, m: &conduit_ir::Module, stack_switching: bool) -> Result<(), String> {
    let with = if stack_switching {
        "with stack switching"
    } else {
        "without stack switching"
    };
    let names: Vec<(FuncId, String)> = m.funcs.keys().map(|&f| (f, f.to_string())).collect();
    let exports: Vec<(FuncId, &str)> = names.iter().map(|(f, n)| (*f, n.as_str())).collect();
    let options = conduit_wasm::linear::Options { stack_switching };
    let bytes = conduit_wasm::linear::emit_module_with(m, &exports, &options)
        .map_err(|e| format!("lowering {with} fails: {e}"))?;
    let mut features = WasmFeatures::default() - WasmFeatures::GC - WasmFeatures::STACK_SWITCHING;
    if stack_switching {
        features |= WasmFeatures::STACK_SWITCHING;
    }
    if let Err(e) = wasmparser::Validator::new_with_features(features).validate_all(&bytes) {
        let wat = wasmprinter::print_bytes(&bytes).unwrap_or_default();
        return Err(format!(
            "the Wasm {with} doesn't validate without GC: {e}\n{wat}"
        ));
    }
    let (mut store, instance) = instantiate(&bytes, stack_switching)?;
    for run in p.runs(m)? {
        let results = m.funcs.get(&run.func).map_or(&[][..], |f| &f.results[..]);
        let got = call(&mut store, &instance, run.func, run.args, results);
        run.check(p, got).map_err(|e| format!("{with}: {e}"))?;
    }
    Ok(())
}
