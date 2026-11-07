//! What the test runners of this crate share: running Wasm in Wasmtime.

use wasmtime::{Engine, Instance, Store};

#[allow(unused_imports)]
pub use conduit_test::wasm::call;

/// Instantiates `bytes`, with stack switching if `stack_switching`.
pub fn instantiate(bytes: &[u8], stack_switching: bool) -> Result<(Store<()>, Instance), String> {
    let mut config = conduit_test::wasm::config();
    config
        .wasm_gc(true)
        .wasm_function_references(true)
        .wasm_stack_switching(stack_switching);
    let engine = Engine::new(&config).map_err(|e| e.to_string())?;
    let module = wasmtime::Module::new(&engine, bytes).map_err(|e| e.to_string())?;
    let linker = conduit_test::wasm::linker(&engine)?;
    let mut store = Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| e.to_string())?;
    Ok((store, instance))
}
