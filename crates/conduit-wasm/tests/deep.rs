//! Functions whose blocks nest deep, as a frontend's do when it puts the
//! rest of a function in the join point of each `if`: every stage must
//! handle them in a test thread's stack, and in time linear in the depth.
//! Printing is left out, since the text of a deep nest is as long as the
//! depth squared, for its indentation alone. The Wasm is validated at
//! full depth, but only run at a smaller one, since engines take memory
//! quadratic in how deep a function's blocks nest to compile it.

use conduit_ir::{Module, text};
use wasmparser::WasmFeatures;

mod support;

/// How deep the blocks nest.
const DEPTH: usize = 20_000;

/// How deep they nest in what Wasmtime runs.
const RUN_DEPTH: usize = 1_000;

/// What `main(0)` gives: `joins` counts up to the depth, the handler's
/// clause adds the 1 it resumes with and 1 after, and `cases(0)` is 0.
fn expected(depth: usize) -> i32 {
    depth as i32 + 2
}

/// `joins`: a join point per level, which the level's `case` jumps to
/// from both arms, with the rest of the function in its body; `cases`: a
/// `case` per level, nested in the default arm of the one before. `joins`
/// performs before its join points, not under them: evidence passing
/// lifts each join point in scope where a `perform` may yield into a
/// function, which takes time quadratic in how many there are.
fn source(depth: usize) -> String {
    let mut s = String::from(
        "effect Ask {
  () -> (i32)
}
handler later for Ask env() ops(ask) ret done
fn ask(%k: cont(i32) -> (i32)) -> (i32) {
  let %r: i32 = resume(%k, 1i32)
  let %r1: i32 = add(%r, 1i32)
  jump ret(%r1)
}
fn done(%r: i32) -> (i32) {
  jump ret(%r)
}
fn joins(%x0: i32) -> (i32) does Ask {
let %a: i32 = perform Ask.0()
",
    );
    for k in 1..=depth {
        s.push_str(&format!("join j{k}(%x{k}: i32) =\n"));
    }
    s.push_str(&format!("let %r: i32 = add(%x{depth}, %a)\njump ret(%r)\n"));
    for k in (1..=depth).rev() {
        let p = k - 1;
        s.push_str(&format!(
            "in case %x{p} {{\n0 => jump j{k}(1i32)\n_ =>\nlet %y{k}: i32 = add(%x{p}, 1i32)\njump j{k}(%y{k})\n}}\n"
        ));
    }
    s.push_str("}\nfn cases(%c0: i32) -> (i32) {\n");
    for k in 1..=depth {
        let p = k - 1;
        s.push_str(&format!(
            "case %c{p} {{\n0 => jump ret({p}i32)\n_ =>\nlet %c{k}: i32 = sub(%c{p}, 1i32)\n"
        ));
    }
    s.push_str("jump ret(-1i32)\n");
    s.push_str(&"}\n".repeat(depth));
    s.push_str(
        "}
fn main(%x: i32) -> (i32) {
  let %b: imm fn() -> (i32) does Ask = closure joins(%x)
  let %r: i32 = handle later(%b)
  let %c: i32 = call cases(%x)
  let %s: i32 = add(%r, %c)
  jump ret(%s)
}
",
    );
    s
}

fn module(depth: usize) -> Module {
    text::parse(&source(depth)).unwrap_or_else(|e| panic!("{e}"))
}

fn main_of(m: &Module) -> conduit_ir::FuncId {
    text::find_func(m, "main").unwrap()
}

/// Runs `main(0)` in the evaluator.
fn eval(m: &Module, counting: bool) -> i32 {
    let mut machine = conduit_eval::Machine::new(m, conduit_eval::NoHost);
    machine.counting = counting;
    let r = machine
        .call(main_of(m), vec![conduit_eval::Value::I32(0)])
        .unwrap();
    match r[..] {
        [conduit_eval::Value::I32(n)] => n,
        _ => panic!("expected an i32, got {r:?}"),
    }
}

/// `m` as Wasm, lowered with `options`, and validated.
fn emit(m: &Module, options: conduit_wasm::Options) -> Vec<u8> {
    let bytes = conduit_wasm::emit_module_with(m, &[(main_of(m), "main")], &options).unwrap();
    let mut features = WasmFeatures::default() - WasmFeatures::GC;
    if options.stack_switching {
        features |= WasmFeatures::STACK_SWITCHING;
    }
    wasmparser::Validator::new_with_features(features)
        .validate_all(&bytes)
        .unwrap();
    bytes
}

/// Runs `main(0)` of `bytes` in Wasmtime.
fn run(bytes: &[u8], stack_switching: bool) -> i32 {
    let (mut store, instance) = support::instantiate(bytes, stack_switching).unwrap();
    let f = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    f.call(&mut store, 0).unwrap()
}

/// Lowers `module(depth)` with `options` and validates it, and at
/// [`RUN_DEPTH`], runs it too.
fn wasm(prepare: impl Fn(&mut Module), options: conduit_wasm::Options) {
    for depth in [DEPTH, RUN_DEPTH] {
        let mut m = module(depth);
        prepare(&mut m);
        let bytes = emit(&m, options);
        if depth == RUN_DEPTH {
            assert_eq!(run(&bytes, options.stack_switching), expected(depth));
        }
    }
}

/// Lowers `m`'s effects, inserts reuse and counts, and specializes drops.
fn counted(m: &mut Module) {
    conduit_effects::lower_effects(m).unwrap();
    conduit_ir::verify::verify_module(m).unwrap();
    conduit_opt::ownership::insert_reuse(m);
    conduit_opt::ownership::insert_counts(m).unwrap();
    conduit_opt::ownership::specialize_drops(m);
    conduit_ir::verify::verify_module(m).unwrap();
}

#[test]
fn parses_and_verifies() {
    conduit_ir::verify::verify_module(&module(DEPTH)).unwrap();
}

#[test]
fn evaluates() {
    assert_eq!(eval(&module(DEPTH), false), expected(DEPTH));
}

#[test]
fn each_pass() {
    let m = module(DEPTH);
    for pass in conduit_opt::PASSES {
        for (&id, f) in &m.funcs {
            let mut f = f.clone();
            pass.run(&m, id, &mut f);
        }
    }
}

#[test]
fn optimizes() {
    let mut m = module(DEPTH);
    conduit_opt::optimize_module(&mut m);
    conduit_ir::verify::verify_module(&m).unwrap();
    assert_eq!(eval(&m, false), expected(DEPTH));
}

#[test]
fn lowers_effects_and_counts() {
    let mut m = module(DEPTH);
    counted(&mut m);
    assert_eq!(eval(&m, true), expected(DEPTH));
    let options = conduit_wasm::Options {
        counts: true,
        cycles: true,
        ..Default::default()
    };
    wasm(counted, options);
}

#[test]
fn lowers_to_wasm() {
    wasm(|_| {}, Default::default());
}

#[test]
fn lowers_to_wasm_switching_stacks() {
    let options = conduit_wasm::Options {
        stack_switching: true,
        ..Default::default()
    };
    wasm(|_| {}, options);
}
