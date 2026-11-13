//! Exceptions from the host, caught by handlers for `throw`, and `raise`s
//! that nothing handles, thrown to the host (#64, `docs/ir/exceptions.md`),
//! with handlers lowered directly, and by evidence passing, which catches
//! what the host throws at the externs that throw (#65).
//!
//! A second module stands in for JavaScript: it exports a tag of one
//! `externref`, as `WebAssembly.JSTag` is, which `throw`'s link names, and
//! a function that throws its argument with it.

use conduit_ir::Module;
use conduit_wasm::Options;
use wasmtime::{Caller, Engine, ExternRef, Instance, Linker, Rooted, Store, TypedFunc, Val};

const THROWER: &str = r#"(module
  (tag $t (export "tag") (param externref))
  (func (export "boom") (param externref) (throw $t (local.get 0))))"#;

const MODULE: &str = r#"
data Pair {
  (i32, i32)
}

#[link(wasm, module = "thrower", name = "tag")]
effect throw

#[link(wasm, module = "thrower", name = "boom")]
extern boom(nullable host) -> () does throw

#[link(wasm, module = "host", name = "wrap")]
extern wrap(i32) -> (host)

#[link(wasm, module = "host", name = "unwrap_or")]
extern unwrap_or(nullable host, i32) -> (i32)

// Calls the export `raising`, without catching what it throws.
#[link(wasm, module = "host", name = "call_raising")]
extern call_raising(i32) -> (i32) does throw

// Gives what was thrown, negated.
handler catch for throw env() ops(caught) ret fine

fn caught(%e: nullable host, %k: cont() -> (i32)) -> (i32) {
  let %v: i32 = call_extern unwrap_or(%e, 1000i32)
  let %r: i32 = sub(0i32, %v)
  jump ret(%r)
}

fn fine(%r: i32) -> (i32) {
  jump ret(%r)
}

// Throws `n` through the host unless it's 0, while owning a pair.
fn middle(%n: i32) -> (i32) does throw {
  let %p: mut Pair = new Pair.0(%n, %n)
  let %zero: i32 = eq(%n, 0i32)
  case %zero {
    0 =>
      let %h: host = call_extern wrap(%n)
      let = call_extern boom(%h)
      jump ret(1i32)
    _ =>
      let %x: i32 = get Pair.0.0(%p)
      jump ret(%x)
  }
}

#[export(wasm)]
fn catching(%n: i32) -> (i32) {
  let %q: mut Pair = new Pair.0(%n, %n)
  let %b: imm fn() -> (i32) does throw = closure middle(%n)
  let %r: i32 = handle catch(%b)
  let %y: i32 = get Pair.0.1(%q)
  let %d: i32 = sub(%y, %n)
  let %s: i32 = add(%r, %d)
  jump ret(%s)
}

// Raises `n`, which nothing here handles.
#[export(wasm)]
fn raising(%n: i32) -> (i32) does throw {
  let %h: host = call_extern wrap(%n)
  let = perform throw.0(%h)
  jump ret(0i32)
}

fn round_trip(%n: i32) -> (i32) does throw {
  let %r: i32 = call_extern call_raising(%n)
  jump ret(%r)
}

// What `raising` throws goes through the host, and back.
#[export(wasm)]
fn through_the_host(%n: i32) -> (i32) {
  let %b: imm fn() -> (i32) does throw = closure round_trip(%n)
  let %r: i32 = handle catch(%b)
  jump ret(%r)
}
"#;

fn instantiate(way: Way) -> (Store<()>, Instance) {
    let Way { options, lowered } = way;
    let mut m: Module = conduit_ir::text::parse(MODULE).unwrap();
    conduit_ir::verify::verify_module(&m).unwrap();
    if lowered {
        conduit_effects::lower_effects(&mut m).unwrap();
        if let Err(errors) = conduit_ir::verify::verify_module(&m) {
            let errors: Vec<String> = errors.iter().map(|e| e.named(&m)).collect();
            panic!("{}\n\n{}", errors.join("\n"), conduit_ir::text::print(&m));
        }
        assert!(
            !m.funcs.values().any(|f| f.body.all_lets().any(|l| matches!(
                l.op,
                conduit_ir::Op::CallExtern(e) if m.externs[&e].sig.row.throws()
            ))),
            "a throwing extern is still called without catching"
        );
    }
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
    linker
        .func_wrap(
            "host",
            "wrap",
            |mut c: Caller<'_, ()>, n: i32| -> wasmtime::Result<Rooted<ExternRef>> {
                ExternRef::new(&mut c, n)
            },
        )
        .unwrap();
    linker
        .func_wrap(
            "host",
            "unwrap_or",
            |c: Caller<'_, ()>, r: Option<Rooted<ExternRef>>, d: i32| -> wasmtime::Result<i32> {
                let Some(r) = r else { return Ok(d) };
                Ok(r.data(&c)?
                    .and_then(|d| d.downcast_ref::<i32>())
                    .copied()
                    .unwrap_or(d))
            },
        )
        .unwrap();
    // An error from the export, the exception, goes on through the host.
    linker
        .func_wrap(
            "host",
            "call_raising",
            |mut c: Caller<'_, ()>, n: i32| -> wasmtime::Result<i32> {
                let raising = c.get_export("raising").unwrap().into_func().unwrap();
                let mut out = [Val::I32(0)];
                raising.call(&mut c, &[Val::I32(n)], &mut out)?;
                Ok(out[0].unwrap_i32())
            },
        )
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

/// A way to lower the module.
#[derive(Clone, Copy, Debug)]
struct Way {
    options: Options,
    /// Whether its effects are lowered by evidence passing first.
    lowered: bool,
}

/// Each way the module can be lowered: with counts or not, by evidence
/// passing or not, objects on the heap so that `conduit_live` counts them.
fn options() -> Vec<Way> {
    let mut ways = Vec::new();
    for lowered in [false, true] {
        for counts in [false, true] {
            let options = Options {
                counts,
                stack_objects: false,
                ..Options::default()
            };
            ways.push(Way { options, lowered });
        }
    }
    ways
}

#[test]
fn a_handler_for_throw_catches_what_the_host_throws() {
    for way in options() {
        let (mut store, instance) = instantiate(way);
        let catching = func::<i32, i32>(&mut store, &instance, "catching");
        assert_eq!(catching.call(&mut store, 0).unwrap(), 0, "{way:?}");
        assert_eq!(catching.call(&mut store, 5).unwrap(), -5, "{way:?}");
        if way.options.counts {
            let live = instance.get_global(&mut store, "conduit_live").unwrap();
            let before = live.get(&mut store).i32().unwrap();
            for _ in 0..100 {
                assert_eq!(catching.call(&mut store, 5).unwrap(), -5);
            }
            assert_eq!(live.get(&mut store).i32().unwrap(), before, "{way:?}");
        }
    }
}

#[test]
fn a_raise_that_nothing_handles_goes_to_the_host() {
    for way in options() {
        let (mut store, instance) = instantiate(way);
        let raising = func::<i32, i32>(&mut store, &instance, "raising");
        assert!(raising.call(&mut store, 7).is_err(), "{way:?}");
        let exn = store.take_pending_exception().expect("an exception");
        let Val::ExternRef(Some(r)) = exn.field(&mut store, 0).unwrap() else {
            panic!("the exception doesn't hold a host value");
        };
        let thrown = r
            .data(&store)
            .unwrap()
            .and_then(|d| d.downcast_ref::<i32>());
        assert_eq!(thrown, Some(&7));
    }
}

#[test]
fn an_exception_goes_through_the_host_and_back() {
    for way in options() {
        let (mut store, instance) = instantiate(way);
        let through = func::<i32, i32>(&mut store, &instance, "through_the_host");
        assert_eq!(through.call(&mut store, 4).unwrap(), -4, "{way:?}");
    }
}
