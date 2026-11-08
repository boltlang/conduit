//! Calling a test program's functions as Wasm, in Wasmtime, for the
//! runners that do: what they set Wasmtime up with, the host functions,
//! and turning literals into Wasm values and back.

use conduit_ir::{FuncId, Lit, Module, Ty};
use wasmtime::{Caller, Engine, ExternRef, Instance, Linker, Memory, Rooted, Store, Val};

use crate::Outcome;

/// What the Wasm the backend makes needs: tail calls and exceptions, both
/// in Wasm 3.0. Runners add what else they want.
pub fn config() -> wasmtime::Config {
    let mut config = wasmtime::Config::new();
    config
        .wasm_tail_call(true)
        .wasm_exceptions(true)
        .wasm_function_references(true);
    config
}

/// A linker with the host functions test programs get, the same as
/// [`eval::TestHost`](crate::eval::TestHost)'s.
pub fn linker(engine: &Engine) -> Result<Linker<()>, String> {
    let mut linker = Linker::new(engine);
    let e = |e: wasmtime::Error| e.to_string();
    linker
        .func_wrap("test", "double", |n: i32| n.wrapping_mul(2))
        .map_err(e)?;
    // A host value that holds a number, and the number back.
    linker
        .func_wrap(
            "test",
            "wrap",
            |mut c: Caller<'_, ()>, n: i32| -> wasmtime::Result<Rooted<ExternRef>> {
                ExternRef::new(&mut c, n)
            },
        )
        .map_err(e)?;
    // A nullable host value: null for a negative number.
    linker
        .func_wrap(
            "test",
            "maybe",
            |mut c: Caller<'_, ()>, n: i32| -> wasmtime::Result<Option<Rooted<ExternRef>>> {
                if n < 0 {
                    Ok(None)
                } else {
                    Ok(Some(ExternRef::new(&mut c, n)?))
                }
            },
        )
        .map_err(e)?;
    linker
        .func_wrap(
            "test",
            "unwrap_or",
            |c: Caller<'_, ()>, r: Option<Rooted<ExternRef>>, d: i32| -> wasmtime::Result<i32> {
                let Some(r) = r else { return Ok(d) };
                let data = r.data(&c)?;
                data.and_then(|d| d.downcast_ref::<i32>())
                    .copied()
                    .ok_or_else(|| {
                        wasmtime::Error::msg("unwrap_or of an externref that isn't maybe's")
                    })
            },
        )
        .map_err(e)?;
    linker
        .func_wrap(
            "test",
            "unwrap",
            |c: Caller<'_, ()>, r: Option<Rooted<ExternRef>>| -> wasmtime::Result<i32> {
                let r = r.ok_or_else(|| wasmtime::Error::msg("unwrap of a null externref"))?;
                let data = r.data(&c)?;
                data.and_then(|d| d.downcast_ref::<i32>())
                    .copied()
                    .ok_or_else(|| wasmtime::Error::msg("unwrap of an externref that isn't wrap's"))
            },
        )
        .map_err(e)?;
    // The flat-ABI ones, which take addresses into the module's memory.
    linker
        .func_wrap("test", "sum", |mut c: Caller<'_, ()>, at: i32, len: i32| {
            let mem = memory(&mut c);
            sum(mem.data(&c), at, len)
        })
        .map_err(e)?;
    linker
        .func_wrap(
            "test",
            "fill",
            |mut c: Caller<'_, ()>, at: i32, len: i32, b: i32| {
                let mem = memory(&mut c);
                let (at, len) = (at as usize, len as usize);
                mem.data_mut(&mut c)[at..at + len].fill(b as u8);
            },
        )
        .map_err(e)?;
    linker
        .func_wrap(
            "test",
            "gather",
            |mut c: Caller<'_, ()>, iovs: i32, n: i32, out: i32| {
                let mem = memory(&mut c);
                let data = mem.data_mut(&mut c);
                let mut total = 0i32;
                for i in 0..n {
                    let iov = (iovs + 8 * i) as usize;
                    let at = read_i32(data, iov);
                    let len = read_i32(data, iov + 4);
                    total = total.wrapping_add(sum(data, at, len));
                }
                data[out as usize..out as usize + 4].copy_from_slice(&total.to_le_bytes());
                n
            },
        )
        .map_err(e)?;
    linker
        .func_wrap(
            "test",
            "squares",
            |mut c: Caller<'_, ()>, at: i32, n: i32| {
                let mem = memory(&mut c);
                let data = mem.data_mut(&mut c);
                for i in 0..n {
                    let to = (at + 4 * i) as usize;
                    data[to..to + 4].copy_from_slice(&(i * i).to_le_bytes());
                }
            },
        )
        .map_err(e)?;
    linker
        .func_wrap(
            "test",
            "scale",
            |mut c: Caller<'_, ()>, n: i32, out: i32| {
                let mem = memory(&mut c);
                let v = i64::from(n) * 1_000_000_000_000;
                mem.data_mut(&mut c)[out as usize..out as usize + 8]
                    .copy_from_slice(&v.to_le_bytes());
                0
            },
        )
        .map_err(e)?;
    linker.func_wrap("test", "byte", |b: i32| b).map_err(e)?;
    Ok(linker)
}

/// The memory of the module calling a host function.
fn memory(c: &mut Caller<'_, ()>) -> Memory {
    c.get_export("memory")
        .and_then(|m| m.into_memory())
        .expect("the module exports its memory")
}

fn read_i32(data: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

/// The sum of `len` bytes from `at`, as signed.
fn sum(data: &[u8], at: i32, len: i32) -> i32 {
    let (at, len) = (at as usize, len as usize);
    data[at..at + len]
        .iter()
        .fold(0i32, |s, &b| s.wrapping_add(i32::from(b as i8)))
}

/// Calls `m`'s export of `func` with `args`, and gives what it gives, read
/// as `result_tys`, or the trap it stopped with.
pub fn call(
    store: &mut Store<()>,
    instance: &Instance,
    m: &Module,
    func: FuncId,
    args: &[Lit],
    result_tys: &[Ty],
) -> Result<Outcome, String> {
    let name = m
        .exports
        .get(&func)
        .map(|e| e.name())
        .ok_or_else(|| format!("{func} isn't exported"))?;
    let f = instance
        .get_func(&mut *store, name)
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
