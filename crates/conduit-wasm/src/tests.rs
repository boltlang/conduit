//! What the test programs in `tests/programs` can't check: which way
//! handlers are lowered, that the output is deterministic, that freed
//! memory is used again, that cycles are collected, and where the shadow
//! stacks overflow.

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
        cycles: false,
        ..Default::default()
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

/// Runs `main` of `src` with counts, freeing and cycles collected: it must
/// give `expected`, stay in two pages of memory, and leave no object live
/// after a last collection.
fn run_counted(src: &str, expected: i32) {
    run_counted_in(src, expected, true)
}

/// [`run_counted`], of `src` with its counts already if not `count`, and
/// without the limit on memory then.
fn run_counted_in(src: &str, expected: i32, count: bool) {
    let mut m = text::parse(src).unwrap();
    if count {
        conduit_opt::ownership::insert_counts(&mut m).unwrap();
    }
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
        cycles: true,
        ..Default::default()
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
    if count {
        assert!(memory.size(&store) <= 2, "{} pages", memory.size(&store));
    }
    let collect = instance
        .get_typed_func::<(), ()>(&mut store, "conduit_collect")
        .unwrap();
    collect.call(&mut store, ()).unwrap();
    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    assert_eq!(live.get(&mut store).unwrap_i32(), 0);
}

/// A ref that holds a box of itself, built 1000 times, is a cycle each
/// time, which the collector frees.
#[test]
fn cycles_are_collected() {
    run_counted(
        "fn main() -> (i32) {
  join rec go(%i: i32) =
    case %i {
      0 => jump ret(%i)
      _ =>
        let %u: mut any = undefined()
        let %r: ref mut any = ref_new(%u)
        let %a: mut any = box[ref mut any](%r)
        let = ref_set(%r, %a)
        let %i1: i32 = sub(%i, 1i32)
        jump go(%i1)
    }
  in jump go(1000i32)
}",
        0,
    );
}

/// Two refs that hold boxes of each other, both buffered: marking gray from
/// the first takes the second's count to 0, which mustn't free it before
/// it's scanned.
#[test]
fn cycles_of_two_roots_are_collected() {
    run_counted(
        "fn main() -> (i32) {
  join rec go(%i: i32) =
    case %i {
      0 => jump ret(%i)
      _ =>
        let %u: mut any = undefined()
        let %r: ref mut any = ref_new(%u)
        let %v: mut any = undefined()
        let %s: ref mut any = ref_new(%v)
        let %a: mut any = box[ref mut any](%s)
        let %b: mut any = box[ref mut any](%r)
        let = ref_set(%r, %a)
        let = ref_set(%s, %b)
        let %i1: i32 = sub(%i, 1i32)
        jump go(%i1)
    }
  in jump go(1000i32)
}",
        0,
    );
}

/// A cycle of 100,000 refs, each holding a box of the one before and the
/// first holding the last, is collected: the collector's stack is in
/// memory, not Wasm's.
#[test]
fn deep_cycles_are_collected() {
    run_counted_in(
        "fn main() -> (i32) {
  let %u: mut any = undefined()
  let %first: ref mut any = ref_new(%u)
  let = dup(%first)
  join rec go(%i: i32, %prev: ref mut any) =
    case %i {
      0 =>
        let %b: mut any = box[ref mut any](%prev)
        let = ref_set(%first, %b)
        let = drop(%first)
        jump ret(%i)
      _ =>
        let %a: mut any = box[ref mut any](%prev)
        let %r: ref mut any = ref_new(%a)
        let %i1: i32 = sub(%i, 1i32)
        jump go(%i1, %r)
    }
  in jump go(100000i32, %first)
}",
        0,
        false,
    );
}

/// An iovec list takes 8 bytes a buffer on the shadow stack, and the
/// second result 8 more: with 64 bytes, a call with 7 buffers fits, every
/// time, since each call pops what it pushed, and one with 8 traps.
#[test]
fn shadow_stack_overflow_traps() {
    let m = conduit_ir::text::parse(
        r#"#[link(wasm, module = "test")]
#[abi(flat)]
extern gather(imm [imm [i8]]) -> (i32, i32)
const hi: imm [i8] = b"hi"
fn main(%n: i32) -> (i32) {
  let %h: imm [i8] = const hi()
  let %list: imm [imm [i8]] = array_fill(%n, %h)
  let %count: i32, %total: i32 = call_extern gather(%list)
  jump ret(%total)
}"#,
    )
    .unwrap();
    let options = crate::Options {
        shadow_stack: 64,
        ..Default::default()
    };
    let main = text::find_func(&m, "main").unwrap();
    let bytes = crate::emit_module_with(&m, &[(main, "main")], &options).unwrap();
    let engine = wasmtime::Engine::new(&conduit_test::wasm::config()).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let linker = conduit_test::wasm::linker(&engine).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    for _ in 0..3 {
        assert_eq!(main.call(&mut store, 7).unwrap(), 7 * (104 + 105));
    }
    let trap = main.call(&mut store, 8).unwrap_err();
    assert!(trap.downcast_ref::<wasmtime::Trap>().is_some(), "{trap:?}");
}

/// Externs in continuations, with stack switching: `gen` runs `body` in a
/// continuation, whose segment fits 3 slots of 8 bytes, while the main
/// stack fits 8. `failing` aborts out of its continuation, which must put
/// the main stack's pointer and limit back.
const SEGMENTS: &str = r#"effect io {}
effect Gen {
  (i32) -> ()
}
effect Fail {
  () -> ()
}
#[link(wasm, module = "test")]
#[abi(flat)]
extern gather(imm [imm [i8]]) -> (i32, i32) does io
const hi: imm [i8] = b"hi"
handler summing for Gen does io + Fail env() ops(sum_yield) ret sum_done
handler or_default for Fail env(i32) ops(fail) ret or_default_done
fn sum_yield(%x: i32, %k: cont() -> (i32) does io + Fail) -> (i32) does io + Fail {
  let %rest: i32 = resume(%k)
  let %r: i32 = add(%rest, %x)
  jump ret(%r)
}
fn sum_done(%r: i32) -> (i32) {
  jump ret(%r)
}
fn fail(%d: i32, %k: cont() -> (i32)) -> (i32) {
  jump ret(%d)
}
fn or_default_done(%d: i32, %r: i32) -> (i32) {
  jump ret(%r)
}
fn total(%n: i32) -> (i32) does io {
  let %h: imm [i8] = const hi()
  let %list: imm [imm [i8]] = array_fill(%n, %h)
  let %c: i32, %t: i32 = call_extern gather(%list)
  jump ret(%t)
}
fn body(%n: i32) -> (i32) does Gen + io + Fail {
  let %t: i32 = call total(%n)
  let = perform Gen.0(%t)
  jump ret(0i32)
}
fn failing_body(%n: i32) -> (i32) does Gen + io + Fail {
  let %t: i32 = call total(%n)
  let = perform Gen.0(%t)
  let = perform Fail.0()
  jump ret(0i32)
}
fn gen(%n: i32) -> (i32) does io + Fail {
  let %b: imm fn() -> (i32) does Gen + io + Fail = closure body(%n)
  let %r: i32 = handle summing(%b)
  jump ret(%r)
}
fn gen_failing(%n: i32) -> (i32) does io + Fail {
  let %b: imm fn() -> (i32) does Gen + io + Fail = closure failing_body(%n)
  let %r: i32 = handle summing(%b)
  jump ret(%r)
}
fn main(%n: i32) -> (i32) does io {
  let %b: imm fn() -> (i32) does io + Fail = closure gen(%n)
  let %r: i32 = handle or_default(-1i32, %b)
  jump ret(%r)
}
fn failing(%n: i32, %after: i32) -> (i32) does io {
  let %b: imm fn() -> (i32) does io + Fail = closure gen_failing(%n)
  let %r: i32 = handle or_default(-1i32, %b)
  let %t: i32 = call total(%after)
  let %r1: i32 = add(%r, %t)
  jump ret(%r1)
}"#;

fn instantiate_segments(continuation_stack: u32) -> (wasmtime::Store<()>, wasmtime::Instance) {
    let m = conduit_ir::text::parse(SEGMENTS).unwrap();
    let options = crate::Options {
        stack_switching: true,
        shadow_stack: 64,
        continuation_stack,
        ..Default::default()
    };
    let exports: Vec<_> = ["main", "failing"]
        .iter()
        .map(|&n| (text::find_func(&m, n).unwrap(), n))
        .collect();
    let bytes = crate::emit_module_with(&m, &exports, &options).unwrap();
    let mut config = conduit_test::wasm::config();
    config
        .wasm_gc(true)
        .wasm_function_references(true)
        .wasm_stack_switching(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let linker = conduit_test::wasm::linker(&engine).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module).unwrap();
    (store, instance)
}

#[test]
fn continuations_have_their_own_shadow_stacks() {
    let (mut store, instance) = instantiate_segments(32);
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    let failing = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "failing")
        .unwrap();
    for _ in 0..3 {
        // 2 buffers and the out slot fill the segment.
        assert_eq!(main.call(&mut store, 2).unwrap(), 2 * 209);
        // After an abort out of a continuation, the main stack's 8 slots.
        assert_eq!(failing.call(&mut store, (2, 7)).unwrap(), -1 + 7 * 209);
    }
    let trap = main.call(&mut store, 3).unwrap_err();
    assert!(trap.downcast_ref::<wasmtime::Trap>().is_some(), "{trap:?}");
}

/// A continuation's segment goes back on the free list when its body
/// returns or throws: with 64 KiB segments, many runs would otherwise grow
/// the memory by much more than the few objects they allocate.
#[test]
fn segments_are_reused() {
    let (mut store, instance) = instantiate_segments(64 * 1024);
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    let failing = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "failing")
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    main.call(&mut store, 1).unwrap();
    let before = memory.data_size(&store);
    for _ in 0..1000 {
        main.call(&mut store, 1).unwrap();
        failing.call(&mut store, (1, 1)).unwrap();
    }
    let grown = memory.data_size(&store) - before;
    assert!(grown < 4 << 20, "the memory grew by {grown} bytes");
}

/// With counts and stack switching: one round resumes its continuation
/// later, through a ref, and another drops it without resuming it, whose
/// frame, shadow stack segment and table slot then go back.
const DROPPED: &str = r#"effect io {}
effect Gen {
  (i32) -> ()
}
#[link(wasm, module = "test")]
#[abi(flat)]
extern gather(imm [imm [i8]]) -> (i32, i32) does io
const hi: imm [i8] = b"hi"
handler parking for Gen does io env(i32) ops(park) ret parked
fn park(%resume: i32, %x: i32, %k: cont() -> (i32) does io) -> (i32) does io {
  let %cell: ref cont() -> (i32) does io = ref_new(%k)
  case %resume {
    0 => jump ret(%x)
    _ =>
      let %k2: cont() -> (i32) does io = ref_get(%cell)
      let %r: i32 = resume(%k2)
      jump ret(%r)
  }
}
fn parked(%resume: i32, %r: i32) -> (i32) {
  jump ret(%r)
}
fn body() -> (i32) does Gen + io {
  let %h: imm [i8] = const hi()
  let %list: imm [imm [i8]] = array_fill(1i32, %h)
  let %c: i32, %t: i32 = call_extern gather(%list)
  let = perform Gen.0(%t)
  jump ret(%c)
}
fn main(%n: i32) -> (i32) does io {
  join rec go(%i: i32, %acc: i32) =
    case %i {
      0 => jump ret(%acc)
      _ =>
        let %b: imm fn() -> (i32) does Gen + io = closure body()
        let %resume: i32 = rem.s(%i, 2i32)
        let %r: i32 = handle parking(%resume, %b)
        let %acc1: i32 = add(%acc, %r)
        let %i1: i32 = sub(%i, 1i32)
        jump go(%i1, %acc1)
    }
  in jump go(%n, 0i32)
}"#;

fn instantiate_dropped<T>(
    data: T,
    continuation_stack: u32,
) -> (wasmtime::Store<T>, wasmtime::Instance) {
    let mut m = conduit_ir::text::parse(DROPPED).unwrap();
    conduit_opt::ownership::insert_counts(&mut m).unwrap();
    conduit_opt::ownership::specialize_drops(&mut m);
    let options = crate::Options {
        stack_switching: true,
        counts: true,
        cycles: true,
        continuation_stack,
        ..Default::default()
    };
    let main = text::find_func(&m, "main").unwrap();
    let bytes = crate::emit_module_with(&m, &[(main, "main")], &options).unwrap();
    let mut config = conduit_test::wasm::config();
    config
        .wasm_gc(true)
        .wasm_function_references(true)
        .wasm_stack_switching(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut linker = wasmtime::Linker::new(&engine);
    linker
        .func_wrap(
            "test",
            "gather",
            |mut caller: wasmtime::Caller<'_, T>, iovs: i32, n: i32, out: i32| -> i32 {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut total = 0i32;
                for j in 0..n {
                    let mut iov = [0u8; 8];
                    memory
                        .read(&caller, (iovs + 8 * j) as usize, &mut iov)
                        .unwrap();
                    let (at, len) = (
                        i32::from_le_bytes(iov[..4].try_into().unwrap()),
                        i32::from_le_bytes(iov[4..].try_into().unwrap()),
                    );
                    let mut bytes = vec![0u8; len as usize];
                    memory.read(&caller, at as usize, &mut bytes).unwrap();
                    total += bytes.iter().map(|&b| b as i8 as i32).sum::<i32>();
                }
                memory
                    .write(&mut caller, out as usize, &total.to_le_bytes())
                    .unwrap();
                n
            },
        )
        .unwrap();
    let mut store = wasmtime::Store::new(&engine, data);
    let instance = linker.instantiate(&mut store, &module).unwrap();
    (store, instance)
}

/// Each round's: the dropped ones give 209, the resumed ones 1.
fn dropped_total(n: i32) -> i32 {
    n / 2 + (n - n / 2) * 209
}

/// A continuation's table slot goes back when it's resumed or dropped, so
/// 10,000 of them fit in tables of 100 slots.
#[test]
fn continuation_slots_are_reused() {
    let limits = wasmtime::StoreLimitsBuilder::new()
        .table_elements(100)
        .build();
    let (mut store, instance) = instantiate_dropped(limits, 4096);
    store.limiter(|limits| limits);
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    assert_eq!(
        main.call(&mut store, 10_000).unwrap(),
        dropped_total(10_000)
    );
    let collect = instance
        .get_typed_func::<(), ()>(&mut store, "conduit_collect")
        .unwrap();
    collect.call(&mut store, ()).unwrap();
    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    assert_eq!(live.get(&mut store).unwrap_i32(), 0);
}

/// A dropped continuation's segment goes back too: with 64 KiB segments,
/// 1000 of them would otherwise take 64 MB.
#[test]
fn dropped_continuations_give_their_segments_back() {
    let (mut store, instance) = instantiate_dropped((), 64 * 1024);
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    main.call(&mut store, 2).unwrap();
    let before = memory.data_size(&store);
    assert_eq!(main.call(&mut store, 1000).unwrap(), dropped_total(1000));
    let grown = memory.data_size(&store) - before;
    assert!(grown < 4 << 20, "the memory grew by {grown} bytes");
}
