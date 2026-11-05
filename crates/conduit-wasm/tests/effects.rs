//! Lowers the effects of every test program with `run` checks by evidence
//! passing (see `conduit_opt::effects`), then lowers it to Wasm, which
//! must validate without stack switching, and runs its `run` checks with
//! Wasmtime without it.

mod support;

use conduit_ir::FuncId;
use conduit_test::{CheckKind, Program};
use support::{call, instantiate};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_) | CheckKind::WasmSkip(_))) && p.has_runs()
}

fn test(p: &Program) -> Result<(), String> {
    let orig = p.module()?;
    let mut m = orig.clone();
    conduit_opt::effects::lower_effects(&mut m)?;
    let names: Vec<(FuncId, String)> = m.funcs.keys().map(|&f| (f, f.to_string())).collect();
    let exports: Vec<(FuncId, &str)> = names.iter().map(|(f, n)| (*f, n.as_str())).collect();
    let bytes =
        conduit_wasm::emit_module(&m, &exports).map_err(|e| format!("lowering fails: {e}"))?;
    if let Err(e) = wasmparser::Validator::new().validate_all(&bytes) {
        let wat = wasmprinter::print_bytes(&bytes).unwrap_or_default();
        return Err(format!(
            "the Wasm doesn't validate without stack switching: {e}\n{wat}"
        ));
    }
    let (mut store, instance) = instantiate(&bytes, false)?;
    for run in p.runs(&m)? {
        let results = orig
            .funcs
            .get(&run.func)
            .map_or(&[][..], |f| &f.results[..]);
        let got = call(&mut store, &instance, run.func, run.args, results);
        run.check(p, got)?;
    }
    Ok(())
}
