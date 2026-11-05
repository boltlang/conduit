//! Lowers every test program to Wasm, validates it without the GC and
//! stack switching proposals, compares it with its `<name>.wat` snapshot
//! if it has a `wat` check, and runs its `run` checks with Wasmtime.
//! Programs with `wasm-error` checks must fail to lower with those
//! messages instead.
//!
//! A program with a handler whose clauses aren't all tail-resumptive or
//! aborting is lowered by evidence passing by default, and is lowered and
//! run again with stack switching, which is then allowed.

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
    // A general handler is lowered by evidence passing without stack
    // switching, and by stack switching with it.
    if has_general_handler(&m) {
        let bytes = lower(p, &m, true)?.expect("lowering failed without stack switching too");
        run(p, &m, &bytes, true)?;
    }
    Ok(())
}

fn has_general_handler(m: &Module) -> bool {
    m.handlers.values().any(|h| {
        h.ops.iter().any(|c| {
            m.funcs.get(c).is_some_and(|f| {
                conduit_ir::clause::plain_call(f).is_none()
                    && conduit_ir::clause::aborting(f).is_none()
            })
        })
    })
}

fn with(stack_switching: bool) -> &'static str {
    if stack_switching {
        "with stack switching"
    } else {
        "without stack switching"
    }
}

/// The Wasm of `m`, validated, with every function exported under its
/// name, or `None` if lowering failed as the `wasm-error` checks expect.
fn lower(p: &Program, m: &Module, stack_switching: bool) -> Result<Option<Vec<u8>>, String> {
    let names: Vec<(FuncId, String)> = m.funcs.keys().map(|&f| (f, f.to_string())).collect();
    let exports: Vec<(FuncId, &str)> = names.iter().map(|(f, n)| (*f, n.as_str())).collect();
    let expected = p.messages(|k| match k {
        CheckKind::WasmError(m) => Some(m),
        _ => None,
    });
    let options = Options { stack_switching };
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
