//! Lowers every test program with `run` checks with the linear memory
//! backend, which lowers its effects first, validates the Wasm without
//! the GC, exception handling and stack switching proposals, and runs its
//! `run` checks with Wasmtime.

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
    let names: Vec<(FuncId, String)> = m.funcs.keys().map(|&f| (f, f.to_string())).collect();
    let exports: Vec<(FuncId, &str)> = names.iter().map(|(f, n)| (*f, n.as_str())).collect();
    let bytes = conduit_wasm::linear::emit_module(&m, &exports)
        .map_err(|e| format!("lowering fails: {e}"))?;
    let features = WasmFeatures::default()
        - WasmFeatures::GC
        - WasmFeatures::EXCEPTIONS
        - WasmFeatures::STACK_SWITCHING;
    if let Err(e) = wasmparser::Validator::new_with_features(features).validate_all(&bytes) {
        let wat = wasmprinter::print_bytes(&bytes).unwrap_or_default();
        return Err(format!("the Wasm doesn't validate without GC: {e}\n{wat}"));
    }
    let (mut store, instance) = instantiate(&bytes, false)?;
    for run in p.runs(&m)? {
        let results = m.funcs.get(&run.func).map_or(&[][..], |f| &f.results[..]);
        let got = call(&mut store, &instance, run.func, run.args, results);
        run.check(p, got)?;
    }
    Ok(())
}
