//! Exceptions and traps that unwind through Conduit's Wasm, which the host
//! catches and goes on from, as a page goes on to its next event (#40):
//! the module's state must be as it was for whoever called it. See
//! `docs/internals/wasm-unwinding.md`.
//!
//! A second module throws an exception of its own, as JavaScript would,
//! and the host catches it where it calls back into Conduit.

use conduit_ir::Module;
use conduit_wasm::Options;
use wasmtime::{Caller, Engine, Instance, Linker, Store, TypedFunc};

/// What the host imports from: `boom` throws an exception that isn't
/// Conduit's.
const THROWER: &str = r#"(module (tag $t) (func (export "boom") (throw $t)))"#;

const MODULE: &str = r#"
data Pair {
  (i32, i32)
}

effect Ask {
  () -> (i32)
}

effect Fail {
  () -> (i32)
}

// Tail-resumptive, so its frames are on the handler stack.
handler answer for Ask env() ops(ask) ret same

// Aborting. What it handles may throw, which it lets through.
handler stop for Fail does throw env() ops(fail) ret same

fn ask(%k: cont(i32) -> (i32)) -> (i32) {
  let %r: i32 = resume(%k, 42i32)
  jump ret(%r)
}

fn fail(%k: cont(i32) -> (i32) does throw) -> (i32) {
  jump ret(0i32)
}

fn same(%r: i32) -> (i32) {
  jump ret(%r)
}

#[link(wasm, module = "thrower", name = "boom")]
extern boom() -> () does throw

#[link(wasm, module = "host", name = "call_inner")]
extern call_inner() -> (i32)

// An object on the shadow stack, then an exception.
#[export(wasm)]
fn inner() -> () does throw {
  let %p: mut Pair = new Pair.0(1i32, 2i32)
  let %x: i32 = get Pair.0.0(%p)
  let = call_extern boom()
  jump ret()
}

// Calls `inner` through the host, which catches what it throws, then
// performs, under the handler it ran in.
fn body() -> (i32) does Ask {
  let %failed: i32 = call_extern call_inner()
  let %a: i32 = perform Ask.0()
  let %s: i32 = add(%a, %failed)
  jump ret(%s)
}

#[export(wasm)]
fn outer() -> (i32) {
  let %b: imm fn() -> (i32) does Ask = closure body()
  let %r: i32 = handle answer(%b)
  jump ret(%r)
}

// The exception goes through an aborting handler.
fn risky() -> (i32) does Fail + throw {
  let = call_extern boom()
  let %x: i32 = perform Fail.0()
  jump ret(%x)
}

#[export(wasm)]
fn guarded() -> (i32) does throw {
  let %b: imm fn() -> (i32) does Fail + throw = closure risky()
  let %r: i32 = handle stop(%b)
  jump ret(%r)
}

// Frames that own objects while an extern throws (#59): the one that
// calls it, and one whose row has nothing but `throw`.
fn middle(%n: i32) -> (i32) does throw {
  let %p: mut Pair = new Pair.0(%n, %n)
  let = call_extern boom()
  let %x: i32 = get Pair.0.0(%p)
  jump ret(%x)
}

#[export(wasm)]
fn owning(%n: i32) -> (i32) does throw {
  let %q: mut Pair = new Pair.0(%n, %n)
  let %x: i32 = call middle(%n)
  let %y: i32 = get Pair.0.1(%q)
  let %r: i32 = add(%x, %y)
  jump ret(%r)
}

// An object on the shadow stack, then a trap.
#[export(wasm)]
fn trapping(%n: i32) -> (i32) {
  let %p: mut Pair = new Pair.0(%n, %n)
  let %x: i32 = get Pair.0.0(%p)
  trap
}
"#;

/// An instance of `MODULE`, lowered with `options`, after inserting counts
/// if they count.
fn instantiate(options: Options) -> (Store<()>, Instance) {
    let mut m: Module = conduit_ir::text::parse(MODULE).unwrap();
    conduit_ir::verify::verify_module(&m).unwrap();
    if options.counts {
        conduit_opt::ownership::insert_counts(&mut m).unwrap();
    }
    let bytes = conduit_wasm::emit_module_with(&m, &options).unwrap();
    let engine = Engine::new(&conduit_test::wasm::config()).unwrap();
    let thrower = wasmtime::Module::new(&engine, THROWER).unwrap();
    let module = wasmtime::Module::new(&engine, bytes).unwrap();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    let thrower = linker.instantiate(&mut store, &thrower).unwrap();
    linker.instance(&mut store, "thrower", thrower).unwrap();
    // Calls `inner`, and catches what it throws, as a host's event loop
    // would: 1 if it failed.
    linker
        .func_wrap("host", "call_inner", |mut c: Caller<'_, ()>| -> i32 {
            let inner = c.get_export("inner").unwrap().into_func().unwrap();
            i32::from(inner.call(&mut c, &[], &mut []).is_err())
        })
        .unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    (store, instance)
}

fn func<P: wasmtime::WasmParams, R: wasmtime::WasmResults>(
    store: &mut Store<()>,
    instance: &Instance,
    name: &str,
) -> TypedFunc<P, R> {
    instance.get_typed_func(&mut *store, name).unwrap()
}

/// The pages of memory the instance has.
fn pages(store: &mut Store<()>, instance: &Instance) -> u64 {
    instance
        .get_memory(&mut *store, "memory")
        .unwrap()
        .size(&*store)
}

/// How many times to unwind: enough that what each leaked would make
/// memory grow.
const ROUNDS: usize = 20_000;

#[test]
fn an_exception_caught_inside_leaves_the_handlers() {
    for counts in [false, true] {
        let (mut store, instance) = instantiate(Options {
            counts,
            ..Options::default()
        });
        let outer = func::<(), i32>(&mut store, &instance, "outer");
        // The handler is still found after the host caught what `inner`
        // threw, and the shadow stack it pushed on goes back.
        assert_eq!(outer.call(&mut store, ()).unwrap(), 43, "counts: {counts}");
        let before = pages(&mut store, &instance);
        for _ in 0..ROUNDS {
            assert_eq!(outer.call(&mut store, ()).unwrap(), 43);
        }
        // Without counts, nothing is freed, so memory grows anyway.
        if counts {
            assert_eq!(pages(&mut store, &instance), before);
        }
    }
}

#[test]
fn an_exception_through_an_aborting_handler_frees_its_frame() {
    let (mut store, instance) = instantiate(Options {
        counts: true,
        ..Options::default()
    });
    let guarded = func::<(), i32>(&mut store, &instance, "guarded");
    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    assert!(guarded.call(&mut store, ()).is_err());
    let before = live.get(&mut store).i32().unwrap();
    for _ in 0..100 {
        assert!(guarded.call(&mut store, ()).is_err());
    }
    assert_eq!(live.get(&mut store).i32().unwrap(), before);
}

#[test]
fn a_trap_is_undone_by_conduit_reset() {
    let (mut store, instance) = instantiate(Options::default());
    let trapping = func::<i32, i32>(&mut store, &instance, "trapping");
    let reset = func::<(), ()>(&mut store, &instance, "conduit_reset");
    let outer = func::<(), i32>(&mut store, &instance, "outer");
    assert!(trapping.call(&mut store, 1).is_err());
    reset.call(&mut store, ()).unwrap();
    let before = pages(&mut store, &instance);
    for _ in 0..ROUNDS {
        assert!(trapping.call(&mut store, 1).is_err());
        reset.call(&mut store, ()).unwrap();
    }
    assert_eq!(pages(&mut store, &instance), before);
    assert_eq!(outer.call(&mut store, ()).unwrap(), 43);

    // Without it, what each trap left on the shadow stack stays there.
    let (mut store, instance) = instantiate(Options::default());
    let trapping = func::<i32, i32>(&mut store, &instance, "trapping");
    let before = pages(&mut store, &instance);
    for _ in 0..ROUNDS {
        assert!(trapping.call(&mut store, 1).is_err());
    }
    assert!(pages(&mut store, &instance) > before);
}

#[test]
fn an_exception_drops_what_the_frames_it_unwinds_own() {
    // On the heap, so that `conduit_live` counts them.
    let (mut store, instance) = instantiate(Options {
        counts: true,
        stack_objects: false,
        ..Options::default()
    });
    let owning = func::<i32, i32>(&mut store, &instance, "owning");
    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    let before = live.get(&mut store).i32().unwrap();
    for _ in 0..100 {
        assert!(owning.call(&mut store, 1).is_err());
    }
    assert_eq!(live.get(&mut store).i32().unwrap(), before);
}
