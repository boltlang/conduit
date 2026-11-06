//! What the test programs in `tests/programs` can't check: which way
//! handlers are lowered, and that the output is deterministic.

use conduit_ir::text;

fn names(src: &str) -> String {
    let m = text::parse(src).unwrap();
    let bytes = crate::emit_module(&m, &[]).unwrap();
    wasmprinter::print_bytes(&bytes).unwrap()
}

/// Handlers whose clauses are all tail-resumptive or aborting are lowered
/// directly, without evidence passing's runtime.
#[test]
fn direct_handlers() {
    let wat = names(include_str!(
        "../../../tests/programs/handlers/state.conduit"
    ));
    assert!(!wat.contains("$lookup"), "{wat}");
    let wat = names(include_str!(
        "../../../tests/programs/handlers/aborting.conduit"
    ));
    assert!(!wat.contains("$lookup"), "{wat}");
    assert!(wat.contains("throw"), "{wat}");
}

/// A clause that does something after resuming needs evidence passing.
#[test]
fn general_handlers() {
    let wat = names(include_str!(
        "../../../tests/programs/handlers/not_tail_resumptive.conduit"
    ));
    assert!(wat.contains("$lookup"), "{wat}");
}

/// With stack switching, a general handler suspends instead.
#[test]
fn general_handlers_switching() {
    let m = text::parse(include_str!(
        "../../../tests/programs/handlers/not_tail_resumptive.conduit"
    ))
    .unwrap();
    let options = crate::Options {
        stack_switching: true,
        counts: false,
    };
    let bytes = crate::emit_module_with(&m, &[], &options).unwrap();
    let wat = wasmprinter::print_bytes(&bytes).unwrap();
    assert!(!wat.contains("$lookup"), "{wat}");
    assert!(wat.contains("suspend"), "{wat}");
    assert!(wat.contains("cont.new"), "{wat}");
}

#[test]
fn output_is_deterministic() {
    let m = text::parse(include_str!("../../../tests/programs/data/lists.conduit")).unwrap();
    assert_eq!(crate::emit_module(&m, &[]), crate::emit_module(&m, &[]));
}

/// With counts, memory that's freed is used again: building and summing a
/// list of 1000 cells 1000 times, about 16 MB without freeing, stays in
/// the first page or two, and frees every cell.
#[test]
fn freed_memory_is_reused() {
    run_counted(
        "data List {
  ()
  (i32, inherit List)
}

fn build(%n: i32) -> (imm List) {
  case %n {
    0 =>
      let %nil: imm List = new List.0()
      jump ret(%nil)
    _ =>
      let %n1: i32 = sub(%n, 1i32)
      let %rest: imm List = call build(%n1)
      let %list: imm List = new List.1(%n, %rest)
      jump ret(%list)
  }
}

fn sum(%l: imm List) -> (i32) {
  case %l {
    List.0 => jump ret(0i32)
    List.1 =>
      let %head: i32 = get List.1.0(%l)
      let %tail: imm List = get List.1.1(%l)
      let %rest: i32 = call sum(%tail)
      let %r: i32 = add(%head, %rest)
      jump ret(%r)
  }
}

fn main() -> (i32) {
  join rec go(%i: i32, %acc: i32) =
    case %i {
      0 => jump ret(%acc)
      _ =>
        let %l: imm List = call build(1000i32)
        let %s: i32 = call sum(%l)
        let %acc1: i32 = add(%acc, %s)
        let %i1: i32 = sub(%i, 1i32)
        jump go(%i1, %acc1)
    }
  in jump go(1000i32, 0i32)
}",
        1000 * 500_500,
    );
}

/// The same for blocks too big for a size class: arrays of 100 boxes.
#[test]
fn freed_big_blocks_are_reused() {
    run_counted(
        "fn main() -> (i32) {
  join rec go(%i: i32, %acc: i32) =
    case %i {
      0 => jump ret(%acc)
      _ =>
        let %b: imm any = box[i32](%i)
        let %a: mut [imm any] = array_fill(100i32, %b)
        let %n: i32 = array_len(%a)
        let %acc1: i32 = add(%acc, %n)
        let %i1: i32 = sub(%i, 1i32)
        jump go(%i1, %acc1)
    }
  in jump go(1000i32, 0i32)
}",
        100_000,
    );
}

/// Runs `main` of `src` with counts and freeing: it must give `expected`,
/// stay in two pages of memory, and leave no object live.
fn run_counted(src: &str, expected: i32) {
    let mut m = text::parse(src).unwrap();
    conduit_opt::ownership::insert_counts(&mut m).unwrap();
    conduit_opt::ownership::specialize_drops(&mut m);
    let main = *m
        .funcs
        .iter()
        .find(|(_, f)| f.debug.name.as_deref() == Some("main"))
        .unwrap()
        .0;
    let options = crate::Options {
        stack_switching: false,
        counts: true,
    };
    let bytes = crate::emit_module_with(&m, &[(main, "main")], &options).unwrap();
    let mut config = wasmtime::Config::new();
    config.wasm_tail_call(true).wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let f = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .unwrap();
    assert_eq!(f.call(&mut store, ()).unwrap(), expected);
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert!(memory.size(&store) <= 2, "{} pages", memory.size(&store));
    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    assert_eq!(live.get(&mut store).unwrap_i32(), 0);
}
