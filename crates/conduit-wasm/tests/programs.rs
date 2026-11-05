//! Lowers every test program to Wasm, validates it, compares it with its
//! `<name>.wat` snapshot if it has a `wat` check, and runs its `run` checks
//! with Wasmtime. Programs with `wasm-error` checks must fail to lower with
//! those messages instead.

mod support;

use conduit_ir::{FuncId, Module};
use conduit_test::{CheckKind, Program};
use support::{call, instantiate};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_) | CheckKind::WasmSkip(_)))
        && (p.has_runs() || p.has(|k| matches!(k, CheckKind::Wat | CheckKind::WasmError(_))))
}

fn test(p: &Program) -> Result<(), String> {
    let m = p.module()?;
    let Some(bytes) = lower(p, &m)? else {
        return Ok(());
    };
    let features = wasmparser::WasmFeatures::default() | wasmparser::WasmFeatures::STACK_SWITCHING;
    if let Err(e) = wasmparser::Validator::new_with_features(features).validate_all(&bytes) {
        let wat = wasmprinter::print_bytes(&bytes).unwrap_or_default();
        return Err(format!("the Wasm doesn't validate: {e}\n{wat}"));
    }
    if p.has(|k| *k == CheckKind::Wat) {
        let wat = wasmprinter::print_bytes(&bytes).map_err(|e| e.to_string())?;
        p.snapshot("wat", &wat)?;
    }
    if !p.has_runs() {
        return Ok(());
    }
    let (mut store, instance) = instantiate(&bytes, true)?;
    for run in p.runs(&m)? {
        let results = m.funcs.get(&run.func).map_or(&[][..], |f| &f.results[..]);
        let got = call(&mut store, &instance, run.func, run.args, results);
        run.check(p, got)?;
    }
    Ok(())
}

/// The Wasm of `m`, with every function exported under its name, or `None`
/// if lowering failed as the `wasm-error` checks expect.
fn lower(p: &Program, m: &Module) -> Result<Option<Vec<u8>>, String> {
    let names: Vec<(FuncId, String)> = m.funcs.keys().map(|&f| (f, f.to_string())).collect();
    let exports: Vec<(FuncId, &str)> = names.iter().map(|(f, n)| (*f, n.as_str())).collect();
    let expected = p.messages(|k| match k {
        CheckKind::WasmError(m) => Some(m),
        _ => None,
    });
    match conduit_wasm::emit_module(m, &exports) {
        Ok(_) if !expected.is_empty() => Err(format!(
            "lowering should fail with {}, but succeeds",
            expected.join("; ")
        )),
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if expected.is_empty() => Err(format!("lowering fails: {e}")),
        Err(e) => match expected.iter().find(|m| !e.to_string().contains(*m)) {
            Some(m) => Err(format!(
                "lowering fails with {e:?}, which doesn't contain {m:?}"
            )),
            None => Ok(None),
        },
    }
}
