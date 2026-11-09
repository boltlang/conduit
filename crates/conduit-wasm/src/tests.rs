//! What the test programs in `tests/programs` can't check: which way
//! handlers are lowered, that the output is deterministic, that freed
//! memory is used again, that cycles are collected, and where the shadow
//! stacks overflow.

use conduit_ir::text;

fn names(src: &str) -> String {
    let m = text::parse(src).unwrap();
    let bytes = crate::emit_module(&m).unwrap();
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
    let bytes = crate::emit_module_with(&m, &options).unwrap();
    let wat = wasmprinter::print_bytes(&bytes).unwrap();
    assert!(!wat.contains("$lookup"), "{wat}");
    assert!(wat.contains("suspend"), "{wat}");
    assert!(wat.contains("cont.new"), "{wat}");
}

#[test]
fn output_is_deterministic() {
    let m = text::parse(include_str!("../../../tests/programs/data/lists.conduit")).unwrap();
    assert_eq!(crate::emit_module(&m), crate::emit_module(&m));
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

#[export(wasm)]
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
        "#[export(wasm)]
fn main() -> (i32) {
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
    let options = crate::Options {
        stack_switching: false,
        counts: true,
        cycles: true,
        ..Default::default()
    };
    let bytes = crate::emit_module_with(&m, &options).unwrap();
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

/// A count that would pass the header's 28 bits saturates instead: the
/// object turns immortal, and is never freed. `array_fill` puts 2^28 + 1
/// references to `%p` in an array, one more than a count can hold. Dropping
/// the array must leave `%p` alone, so the `new` after it can't take its
/// memory: `%p` still holds 7, and the new object 9. The array takes 1 GiB.
#[test]
fn counts_saturate() {
    let src = "data Cell {
  (i32)
}

#[export(wasm)]
fn main() -> (i32) {
  let %p: imm Cell = new Cell.0(7i32)
  let %a: imm [imm Cell] = array_fill(268435457i32, %p)
  let %n: i32 = array_len(%a)
  let %q: imm Cell = new Cell.0(9i32)
  let %x: i32 = get Cell.0.0(%p)
  let %y: i32 = get Cell.0.0(%q)
  let %x100: i32 = mul(%x, 100i32)
  let %r0: i32 = add(%x100, %y)
  let %big: i32 = gt.s(%n, 268435456i32)
  let %r: i32 = mul(%r0, %big)
  jump ret(%r)
}";
    let mut m = text::parse(src).unwrap();
    conduit_opt::ownership::insert_counts(&mut m).unwrap();
    let options = crate::Options {
        stack_switching: false,
        counts: true,
        cycles: true,
        ..Default::default()
    };
    let bytes = crate::emit_module_with(&m, &options).unwrap();
    let mut config = wasmtime::Config::new();
    config.wasm_tail_call(true).wasm_exceptions(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let f = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .unwrap();
    assert_eq!(f.call(&mut store, ()).unwrap(), 709);
    // `%p` is immortal, so it's left: only it.
    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    assert_eq!(live.get(&mut store).unwrap_i32(), 1);
}

/// A ref that holds a box of itself, built 1000 times, is a cycle each
/// time, which the collector frees.
#[test]
fn cycles_are_collected() {
    run_counted(
        "#[export(wasm)]
fn main() -> (i32) {
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
        "#[export(wasm)]
fn main() -> (i32) {
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
        "#[export(wasm)]
fn main() -> (i32) {
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
/// second result 8 more: with a main stack of 64 bytes, a call with 7
/// buffers fits, and bigger ones chain segments, also bigger than the
/// usual one. Each call pops what it pushed, list included, which doesn't
/// escape, so calling again takes no more memory.
#[test]
fn shadow_stack_grows() {
    let m = conduit_ir::text::parse(
        r#"#[link(wasm, module = "test")]
#[abi(flat)]
extern gather(imm [imm [i8]]) -> (i32, i32)
const hi: imm [i8] = b"hi"
#[export(wasm)]
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
    let bytes = crate::emit_module_with(&m, &options).unwrap();
    let engine = wasmtime::Engine::new(&conduit_test::wasm::config()).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let linker = conduit_test::wasm::linker(&engine).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    for n in [7, 8, 100, 10_000] {
        assert_eq!(main.call(&mut store, n).unwrap(), n * (104 + 105));
    }
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let before = memory.data_size(&store);
    for n in [7, 8, 100, 10_000].repeat(100) {
        assert_eq!(main.call(&mut store, n).unwrap(), n * (104 + 105));
    }
    assert_eq!(memory.data_size(&store), before);
}

/// Externs in continuations, with stack switching: `gen` runs `body` in a
/// continuation, whose first segment fits 2 slots of 8 bytes, while the
/// main stack fits 8, and more chains segments. `failing` aborts out of
/// its continuation, which must put the main stack's pointer and limit
/// back, and give back the continuation's segments. Counted, so that the
/// heap frees what each round makes: the memory then only grows if the
/// shadow stack does.
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
#[export(wasm)]
fn main(%n: i32) -> (i32) does io {
  let %b: imm fn() -> (i32) does io + Fail = closure gen(%n)
  let %r: i32 = handle or_default(-1i32, %b)
  jump ret(%r)
}
#[export(wasm)]
fn failing(%n: i32, %after: i32) -> (i32) does io {
  let %b: imm fn() -> (i32) does io + Fail = closure gen_failing(%n)
  let %r: i32 = handle or_default(-1i32, %b)
  let %t: i32 = call total(%after)
  let %r1: i32 = add(%r, %t)
  jump ret(%r1)
}"#;

/// [`SEGMENTS`] with segments of `continuation_stack` bytes, counting if
/// `counts` says so, which frees what the heap holds.
fn instantiate_segments(
    continuation_stack: u32,
    counts: bool,
) -> (wasmtime::Store<()>, wasmtime::Instance) {
    let mut m = conduit_ir::text::parse(SEGMENTS).unwrap();
    if counts {
        conduit_opt::ownership::insert_counts(&mut m).unwrap();
        conduit_opt::ownership::specialize_drops(&mut m);
    }
    let options = crate::Options {
        stack_switching: true,
        shadow_stack: 64,
        continuation_stack,
        counts,
        cycles: counts,
        ..Default::default()
    };
    let bytes = crate::emit_module_with(&m, &options).unwrap();
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
fn continuations_grow_their_own_shadow_stacks() {
    let (mut store, instance) = instantiate_segments(32, true);
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    let failing = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "failing")
        .unwrap();
    let rounds = |store: &mut wasmtime::Store<()>| {
        for n in [1, 3, 100] {
            assert_eq!(main.call(&mut *store, n).unwrap(), n * 209);
            for after in [7, 100] {
                assert_eq!(
                    failing.call(&mut *store, (n, after)).unwrap(),
                    -1 + after * 209
                );
            }
        }
    };
    rounds(&mut store);
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let before = memory.data_size(&store);
    for _ in 0..100 {
        rounds(&mut store);
    }
    assert_eq!(memory.data_size(&store), before);
}

/// A continuation's segment goes back on the free list when its body
/// returns or throws: with 64 KiB segments, many runs would otherwise grow
/// the memory by much more than the few objects they allocate.
#[test]
fn segments_are_reused() {
    let (mut store, instance) = instantiate_segments(64 * 1024, false);
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
#[export(wasm)]
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
    let bytes = crate::emit_module_with(&m, &options).unwrap();
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

/// An abort skips the pops of the frames it unwinds, so the `handle` that
/// catches it pops back to where it started: else each of these, whose
/// body pushes a 4 KB array and aborts, would leave the array pushed.
#[test]
fn aborts_pop_the_shadow_stack() {
    let m = conduit_ir::text::parse(
        "effect Fail {
  () -> ()
}
handler or_default for Fail env(i32) ops(fail) ret done
fn fail(%d: i32, %k: cont() -> (i32)) -> (i32) {
  jump ret(%d)
}
fn done(%d: i32, %r: i32) -> (i32) {
  jump ret(%r)
}
fn body(%n: i32) -> (i32) does Fail {
  let %xs: mut [i32] = array_fill(%n, 7i32)
  let %x: i32 = array_get(%xs, 0i32)
  let = perform Fail.0()
  jump ret(%x)
}
#[export(wasm)]
fn main(%n: i32) -> (i32) {
  let %b: imm fn() -> (i32) does Fail = closure body(%n)
  let %r: i32 = handle or_default(-1i32, %b)
  jump ret(%r)
}",
    )
    .unwrap();
    let bytes = crate::emit_module(&m).unwrap();
    let engine = wasmtime::Engine::new(&conduit_test::wasm::config()).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(main.call(&mut store, 1000).unwrap(), -1);
    let before = memory.data_size(&store);
    for _ in 0..1000 {
        assert_eq!(main.call(&mut store, 1000).unwrap(), -1);
    }
    let grown = memory.data_size(&store) - before;
    assert!(grown < 1 << 20, "the memory grew by {grown} bytes");
}

/// An array that doesn't escape goes on the shadow stack, and is popped
/// when its function returns: without counts, the heap never frees, so
/// 1000 of these 40 KB arrays would take 40 MB of it. The call isn't in
/// tail position, which would keep the array.
#[test]
fn objects_that_dont_escape_go_on_the_stack() {
    let m = conduit_ir::text::parse(
        "fn sum(%xs: ro [i32]) -> (i32) {
  let %n: i32 = array_len(%xs)
  join rec go(%i: i32, %acc: i32) =
    let %done: i32 = ge.s(%i, %n)
    case %done {
      1 => jump ret(%acc)
      _ =>
        let %x: i32 = array_get(%xs, %i)
        let %acc1: i32 = add(%acc, %x)
        let %i1: i32 = add(%i, 1i32)
        jump go(%i1, %acc1)
    }
  in jump go(0i32, 0i32)
}
#[export(wasm)]
fn main(%n: i32) -> (i32) {
  let %xs: mut [i32] = array_fill(%n, 2i32)
  let %r: i32 = call sum(%xs)
  let %r1: i32 = add(%r, 0i32)
  jump ret(%r1)
}",
    )
    .unwrap();
    let bytes = crate::emit_module(&m).unwrap();
    let engine = wasmtime::Engine::new(&conduit_test::wasm::config()).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let main = instance
        .get_typed_func::<i32, i32>(&mut store, "main")
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(main.call(&mut store, 10_000).unwrap(), 20_000);
    let before = memory.data_size(&store);
    for _ in 0..1000 {
        assert_eq!(main.call(&mut store, 10_000).unwrap(), 20_000);
    }
    assert_eq!(memory.data_size(&store), before);
}

/// Only an extern with a wasm link can be imported: one with another kind
/// of link is for another target.
#[test]
fn only_wasm_links_are_imported() {
    for (link, kind) in [("emu", "emu"), ("c, lib = \"c\"", "c")] {
        let m = conduit_ir::text::parse(&format!(
            "#[link({link})]
extern double(i32) -> (i32)
#[export(wasm)]
fn main(%n: i32) -> (i32) {{
  let %r: i32 = call_extern double(%n)
  jump ret(%r)
}}"
        ))
        .unwrap();
        let error = crate::emit_module(&m).unwrap_err();
        assert!(
            error.to_string().ends_with(&format!(
                "can't import a {kind} extern: only one with a `#[link(wasm, ..)]`"
            )),
            "{error}"
        );
    }
}

/// A list the host gets from one export and passes to another, which only
/// reads it, is the second's to drop: an exported function owns what it's
/// given, as the host owns what it gets back, whatever borrowing would
/// infer for a caller inside the module.
#[test]
fn exports_own_their_arguments() {
    let mut m = text::parse(
        "data List {
  ()
  (i32, inherit List)
}
#[export(wasm)]
fn build(%n: i32) -> (imm List) {
  case %n {
    0 =>
      let %nil: imm List = new List.0()
      jump ret(%nil)
    _ =>
      let %n1: i32 = sub(%n, 1i32)
      let %rest: imm List = call build(%n1)
      let %l: imm List = new List.1(%n, %rest)
      jump ret(%l)
  }
}
#[export(wasm)]
fn sum(%l: imm List) -> (i32) {
  case %l {
    List.0 => jump ret(0i32)
    List.1 =>
      let %x: i32 = get List.1.0(%l)
      let %rest: imm List = get List.1.1(%l)
      let %s: i32 = call sum(%rest)
      let %r: i32 = add(%x, %s)
      jump ret(%r)
  }
}",
    )
    .unwrap();
    conduit_opt::ownership::insert_counts(&mut m).unwrap();
    conduit_opt::ownership::specialize_drops(&mut m);
    let options = crate::Options {
        counts: true,
        ..Default::default()
    };
    let bytes = crate::emit_module_with(&m, &options).unwrap();
    let engine = wasmtime::Engine::new(&conduit_test::wasm::config()).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let build = instance
        .get_typed_func::<i32, i32>(&mut store, "build")
        .unwrap();
    let sum = instance
        .get_typed_func::<i32, i32>(&mut store, "sum")
        .unwrap();
    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    let l = build.call(&mut store, 3).unwrap();
    assert!(live.get(&mut store).unwrap_i32() > 0);
    assert_eq!(sum.call(&mut store, l).unwrap(), 6);
    assert_eq!(live.get(&mut store).unwrap_i32(), 0);
}

/// What the backend can't lower, with what it says: a module that counts,
/// with handlers that need evidence passing, whose effects weren't lowered
/// before its counts were inserted, and exports and externs for another
/// target.
#[test]
fn errors_for_what_it_cant_lower() {
    let message = |src: &str, options: &crate::Options| {
        let m = text::parse(src).unwrap();
        crate::emit_module_with(&m, options).unwrap_err().message
    };
    let general = include_str!("../../../tests/programs/handlers/not_tail_resumptive.conduit");
    let counted = crate::Options {
        stack_switching: false,
        counts: true,
        cycles: false,
        ..Default::default()
    };
    assert!(
        message(general, &counted).contains("must have its effects lowered before its counts"),
        "{}",
        message(general, &counted)
    );
    let plain = crate::Options::default();
    assert_eq!(
        message(
            "#[export(emu, name = \"f\")]\nfn f() -> () { jump ret() }",
            &plain
        ),
        "can't export a function as emu `f`: only with a `#[export(wasm, ..)]`"
    );
    assert_eq!(
        message(
            "#[link(emu)]\nextern double(i32) -> (i32)\nfn f() -> () { jump ret() }",
            &plain
        ),
        "can't import a emu extern: only one with a `#[link(wasm, ..)]`"
    );
}

/// An import's `host` results are `(ref extern)`, which can't be null,
/// and its parameters `externref`, which is what the JS String Builtins
/// check: `substring` is `(externref, i32, i32) -> (ref extern)`. A
/// `nullable host` result is an `externref`.
#[test]
fn host_imports_have_the_builtins_signatures() {
    let wat = names(
        "#[link(wasm, module = \"wasm:js-string\", name = \"substring\")]
extern substring(host, i32, i32) -> (host)
#[link(wasm, module = \"wasm:js-string\", name = \"length\")]
extern length(host) -> (i32)
#[link(wasm, module = \"env\", name = \"find\")]
extern find(nullable host) -> (nullable host)

#[export(wasm)]
fn main(%n: i32) -> (i32) {
  let %none: nullable host = host_null()
  let %found: nullable host = call_extern find(%none)
  let %s: host = host_unwrap(%found)
  let %t: host = call_extern substring(%s, 0i32, %n)
  let %r: i32 = call_extern length(%t)
  jump ret(%r)
}",
    );
    let import = |name: &str| {
        wat.lines()
            .find(|l| l.contains("(import") && l.contains(&format!("\"{name}\"")))
            .unwrap_or_else(|| panic!("no import {name} in\n{wat}"))
            .to_string()
    };
    assert!(
        import("substring").contains("(param externref i32 i32) (result (ref extern))"),
        "{}",
        import("substring")
    );
    assert!(
        import("length").contains("(param externref) (result i32)"),
        "{}",
        import("length")
    );
    assert!(
        import("find").contains("(param externref) (result externref)"),
        "{}",
        import("find")
    );
}

const HOST_EXPORTS: &str = "#[link(wasm, module = \"test\", name = \"wrap\")]
extern wrap(i32) -> (host)
#[link(wasm, module = \"test\", name = \"unwrap\")]
extern unwrap(host) -> (i32)

#[export(wasm, name = \"echo\")]
fn echo(%h: host) -> (host) {
  jump ret(%h)
}

#[export(wasm, name = \"read\")]
fn read(%h: host) -> (i32) {
  let %r: i32 = call_extern unwrap(%h)
  jump ret(%r)
}

#[export(wasm, name = \"make\")]
fn make(%n: i32) -> (host) {
  let %h: host = call_extern wrap(%n)
  jump ret(%h)
}

// The first if it isn't null, and whether it was.
#[export(wasm, name = \"either\")]
fn either(%a: nullable host, %b: host) -> (nullable host, i32) {
  let %null: i32 = host_is_null(%a)
  case %null {
    0 => jump ret(%a, 0i32)
    _ => jump ret(%b, 1i32)
  }
}";

/// Exports take and give host values as the host's own references: each
/// `externref` gets a box, and each box given back is its `externref`,
/// whose box is then dropped, so nothing's left live. A `host` parameter
/// can't be null, and a `nullable host` one can.
#[test]
fn exports_take_and_give_host_values() {
    use wasmtime::{ExternRef, Rooted, Val};
    let mut m = text::parse(HOST_EXPORTS).unwrap();
    conduit_opt::ownership::insert_counts(&mut m).unwrap();
    let options = crate::Options {
        counts: true,
        ..Default::default()
    };
    let bytes = crate::emit_module_with(&m, &options).unwrap();
    let engine = wasmtime::Engine::new(&conduit_test::wasm::config()).unwrap();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let linker = conduit_test::wasm::linker(&engine).unwrap();
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let func =
        |store: &mut wasmtime::Store<()>, name: &str| instance.get_func(&mut *store, name).unwrap();
    let number = |store: &wasmtime::Store<()>, v: &Val| -> i32 {
        let r = v.unwrap_externref().expect("not null");
        *r.data(store)
            .unwrap()
            .unwrap()
            .downcast_ref::<i32>()
            .unwrap()
    };
    let host = |store: &mut wasmtime::Store<()>, n: i32| -> Rooted<ExternRef> {
        ExternRef::new(store, n).unwrap()
    };

    let seven = host(&mut store, 7);
    let mut out = [Val::I32(0)];
    let echo = func(&mut store, "echo");
    echo.call(&mut store, &[Val::ExternRef(Some(seven))], &mut out)
        .unwrap();
    assert_eq!(number(&store, &out[0]), 7);

    let read = func(&mut store, "read");
    read.call(&mut store, &[Val::ExternRef(Some(seven))], &mut out)
        .unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
    // A `host` parameter is a `(ref extern)`, so a null doesn't get in.
    assert!(
        read.call(&mut store, &[Val::ExternRef(None)], &mut out)
            .is_err()
    );

    let make = func(&mut store, "make");
    make.call(&mut store, &[Val::I32(5)], &mut out).unwrap();
    assert_eq!(number(&store, &out[0]), 5);

    let either = func(&mut store, "either");
    let mut two = [Val::I32(0), Val::I32(0)];
    let nine = host(&mut store, 9);
    either
        .call(
            &mut store,
            &[Val::ExternRef(None), Val::ExternRef(Some(nine))],
            &mut two,
        )
        .unwrap();
    assert_eq!((number(&store, &two[0]), two[1].unwrap_i32()), (9, 1));
    either
        .call(
            &mut store,
            &[Val::ExternRef(Some(seven)), Val::ExternRef(Some(nine))],
            &mut two,
        )
        .unwrap();
    assert_eq!((number(&store, &two[0]), two[1].unwrap_i32()), (7, 0));

    let live = instance.get_global(&mut store, "conduit_live").unwrap();
    assert_eq!(live.get(&mut store).unwrap_i32(), 0);
}

const CALLBACKS: &str = "#[link(wasm, module = \"test\", name = \"keep\")]
extern keep(host) -> ()
#[link(wasm, module = \"test\", name = \"unwrap\")]
extern unwrap(host) -> (i32)

#[export(wasm, name = \"main\")]
fn main(%k: i32) -> () {
  let %c: imm fn(i32) -> (i32) = closure plus(%k)
  let %f: host = host_function(%c)
  let = call_extern keep(%f)
  let %r: imm fn(host) -> (i32) = closure read()
  let %g: host = host_function(%r)
  let = call_extern keep(%g)
  jump ret()
}

fn plus(%k: i32, %x: i32) -> (i32) {
  let %y: i32 = add(%k, %x)
  jump ret(%y)
}

fn read(%h: host) -> (i32) {
  let %n: i32 = call_extern unwrap(%h)
  jump ret(%n)
}
";

/// A handler whose clause isn't tail-resumptive, so a module with it
/// lowers its effects by evidence passing, which every closure then takes.
const GENERAL_HANDLER: &str = "
effect Tick {
  () -> ()
}

handler twice for Tick env() ops(tick) ret ticked

fn tick(%k: cont() -> (i32)) -> (i32) {
  let %r: i32 = resume(%k)
  let %r2: i32 = add(%r, 1i32)
  jump ret(%r2)
}

fn ticked() -> (i32) {
  jump ret(0i32)
}
";

/// `host_function` gives the host a JavaScript function, which here the
/// test's own `conduit.function` makes, as Bolt's runtime would: it keeps
/// the trampoline and the closure, and calling the function calls the
/// trampoline with the closure first. Once the host lets go,
/// `conduit_release` drops the closure, so nothing is left live. With a
/// handler that needs evidence passing too, the closures the host gets take
/// no evidence.
#[test]
fn host_functions_call_closures() {
    use std::sync::{Arc, Mutex};
    use wasmtime::{ExternRef, Func, Rooted, Val};
    for with_handler in [false, true] {
        let src = if with_handler {
            format!("{CALLBACKS}{GENERAL_HANDLER}")
        } else {
            CALLBACKS.to_string()
        };
        let mut m = text::parse(&src).unwrap();
        if with_handler {
            conduit_effects::lower_effects(&mut m).unwrap();
            let adapters = m
                .funcs
                .values()
                .filter(|f| f.debug.name.as_deref() == Some("host_adapter"));
            assert_eq!(adapters.count(), 2, "one adapter per signature");
        }
        conduit_opt::ownership::insert_counts(&mut m).unwrap();
        let options = crate::Options {
            counts: true,
            ..Default::default()
        };
        let bytes = crate::emit_module_with(&m, &options).unwrap();
        let engine = wasmtime::Engine::new(&conduit_test::wasm::config()).unwrap();
        let module = wasmtime::Module::new(&engine, &bytes).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let mut linker = conduit_test::wasm::linker(&engine).unwrap();
        let kept: Arc<Mutex<Vec<(Func, i32)>>> = Arc::default();
        linker
            .func_wrap(
                "conduit",
                "function",
                |mut c: wasmtime::Caller<'_, ()>,
                 f: Option<Func>,
                 closure: i32|
                 -> wasmtime::Result<Rooted<ExternRef>> {
                    ExternRef::new(&mut c, (f.expect("a trampoline"), closure))
                },
            )
            .unwrap();
        let keeps = kept.clone();
        linker
            .func_wrap(
                "test",
                "keep",
                move |c: wasmtime::Caller<'_, ()>,
                      r: Option<Rooted<ExternRef>>|
                      -> wasmtime::Result<()> {
                    let data = r.expect("a function").data(&c)?.expect("data");
                    let &(f, closure) = data.downcast_ref::<(Func, i32)>().expect("a function");
                    keeps.lock().unwrap().push((f, closure));
                    Ok(())
                },
            )
            .unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let main = instance.get_func(&mut store, "main").unwrap();
        main.call(&mut store, &[Val::I32(10)], &mut []).unwrap();
        let functions = kept.lock().unwrap().clone();
        let [(plus, c1), (read, c2)] = functions[..] else {
            panic!("{} functions", functions.len())
        };
        let mut out = [Val::I32(0)];
        for x in [5, 7] {
            plus.call(&mut store, &[Val::I32(c1), Val::I32(x)], &mut out)
                .unwrap();
            assert_eq!(
                out[0].unwrap_i32(),
                10 + x,
                "with a handler: {with_handler}"
            );
        }
        let h = ExternRef::new(&mut store, 42i32).unwrap();
        read.call(
            &mut store,
            &[Val::I32(c2), Val::ExternRef(Some(h))],
            &mut out,
        )
        .unwrap();
        assert_eq!(out[0].unwrap_i32(), 42);
        let release = instance.get_func(&mut store, "conduit_release").unwrap();
        for c in [c1, c2] {
            release.call(&mut store, &[Val::I32(c)], &mut []).unwrap();
        }
        let live = instance.get_global(&mut store, "conduit_live").unwrap();
        assert_eq!(
            live.get(&mut store).unwrap_i32(),
            0,
            "with a handler: {with_handler}"
        );
    }
}
