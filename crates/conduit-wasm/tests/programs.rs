//! Lowers every test program to Wasm, validates it, compares it with its
//! `<name>.wat` snapshot if it has a `wat` check, and runs its `run` checks
//! with Wasmtime. Programs with `wasm-error` checks must fail to lower with
//! those messages instead.

use conduit_ir::{FuncId, Lit, Module};
use conduit_test::{CheckKind, Outcome, Program};
use wasmtime::{Engine, Instance, Linker, Store, Val};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_) | CheckKind::WasmSkip(_)))
        && (p.runs().next().is_some()
            || p.has(|k| matches!(k, CheckKind::Wat | CheckKind::WasmError(_))))
}

fn test(p: &Program) -> Result<(), String> {
    let m = p.module()?;
    let Some(bytes) = lower(p, &m)? else {
        return Ok(());
    };
    if let Err(e) = wasmparser::Validator::new().validate_all(&bytes) {
        let wat = wasmprinter::print_bytes(&bytes).unwrap_or_default();
        return Err(format!("the Wasm doesn't validate: {e}\n{wat}"));
    }
    if p.has(|k| *k == CheckKind::Wat) {
        let wat = wasmprinter::print_bytes(&bytes).map_err(|e| e.to_string())?;
        p.snapshot("wat", &wat)?;
    }
    if p.runs().next().is_none() {
        return Ok(());
    }
    let (mut store, instance) = instantiate(&bytes)?;
    for run in p.runs() {
        let got = call(&mut store, &instance, run.func, run.args);
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

fn instantiate(bytes: &[u8]) -> Result<(Store<()>, Instance), String> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_gc(true)
        .wasm_function_references(true)
        .wasm_tail_call(true);
    let engine = Engine::new(&config).map_err(|e| e.to_string())?;
    let module = wasmtime::Module::new(&engine, bytes).map_err(|e| e.to_string())?;
    let mut linker = Linker::new(&engine);
    // The same host functions as `conduit_test::eval::TestHost`.
    linker
        .func_wrap("test", "double", |n: i32| n.wrapping_mul(2))
        .map_err(|e| e.to_string())?;
    let mut store = Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| e.to_string())?;
    Ok((store, instance))
}

fn call(
    store: &mut Store<()>,
    instance: &Instance,
    func: FuncId,
    args: &[Lit],
) -> Result<Outcome, String> {
    let name = func.to_string();
    let f = instance
        .get_func(&mut *store, &name)
        .ok_or_else(|| format!("{name} isn't exported"))?;
    let params: Vec<Val> = args
        .iter()
        .map(|l| match *l {
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
            .map(|v| match v {
                Val::I32(n) => Ok(Lit::I32(n)),
                Val::I64(n) => Ok(Lit::I64(n)),
                Val::F32(bits) => Ok(Lit::F32(bits)),
                Val::F64(bits) => Ok(Lit::F64(bits)),
                _ => Err("gave a reference, but a test can only check scalars".to_string()),
            })
            .collect::<Result<_, _>>()
            .map(Outcome::Values),
        Err(e) if e.downcast_ref::<wasmtime::Trap>().is_some() => Ok(Outcome::Trap(e.to_string())),
        Err(e) => Err(e.to_string()),
    }
}
