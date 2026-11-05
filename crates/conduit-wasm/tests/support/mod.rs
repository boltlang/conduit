//! What the test runners of this crate share: running Wasm in Wasmtime.

use conduit_ir::{FuncId, Lit, Ty};
use conduit_test::Outcome;
use wasmtime::{Engine, Instance, Linker, Store, Val};

/// Instantiates `bytes`, with stack switching if `stack_switching`.
pub fn instantiate(bytes: &[u8], stack_switching: bool) -> Result<(Store<()>, Instance), String> {
    let mut config = wasmtime::Config::new();
    config
        .wasm_gc(true)
        .wasm_function_references(true)
        .wasm_tail_call(true)
        .wasm_exceptions(true)
        .wasm_stack_switching(stack_switching);
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

pub fn call(
    store: &mut Store<()>,
    instance: &Instance,
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
        // An `i8` or `i16` comes back as an `i32` whose low bits are the
        // value.
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
                _ => Err("gave a reference, but a test can only check scalars".to_string()),
            })
            .collect::<Result<_, _>>()
            .map(Outcome::Values),
        Err(e) if e.downcast_ref::<wasmtime::Trap>().is_some() => Ok(Outcome::Trap(e.to_string())),
        Err(e) => Err(e.to_string()),
    }
}
