# The Wasm backend

`conduit-wasm` lowers a verified module to a Wasm module whose values live
in linear memory, from an allocator of its own, so the engine needs no Wasm
GC: Wasm 2.0 with tail calls, exception handling for aborting handlers, and
stack switching only when asked for. What a JavaScript host gives and gets
is in [host/js.md](host/js.md).

`emit_module_with(m, &Options)` takes these options, and `emit_module` the
defaults:

| Option | Default | What it does |
|---|---|---|
| `stack_switching` | off | lowers handlers whose clauses aren't all tail-resumptive or aborting with Wasm stack switching, instead of needing evidence passing first |
| `counts` | off | the module's counts are explicit (`insert_counts` made them), so `dup` and `drop` count and memory is freed |
| `cycles` | off | with `counts`, collects cycles by trial deletion too |
| `shadow_stack` | 64 KiB | the main stack's first segment of the shadow stack |
| `continuation_stack` | 4 KiB | each continuation's first segment, with stack switching |
| `stack_objects` | on | puts objects that don't escape on the shadow stack |

## Control flow

**To Wasm.** Pass arguments in Wasm locals so blocks don't need typed
stack values.

| Conduit | Wasm |
|---|---|
| `join j(..) = D in B` | `block $j  B  end` then `D` |
| `join rec j(..) = D in B` | `block $enter B end` then `loop $j D end` |
| `join rec j1 .., jn` (n > 1) | `loop` plus a `br_table` on a "which label" local |
| `jump j(args)` | set the parameter locals, then `br $j` |
| `jump ret(args)` | `return` (or `return_call` for a tail call) |
| `case` | `br_table` over nested blocks, or an `if` chain |

A label is only in scope inside the `block`/`loop` that implements it, so
every `br` targets an enclosing label, as Wasm requires.

Wasm has no `i8` or `i16` values, so, as in LLVM's Wasm backend, each is
an `i32` whose low 8 or 16 bits are the value, and whose high bits can be
anything. Ops whose low bits only depend on their operands' low bits
(`add`, `sub`, `mul`, `and`, `or`, `xor`, `shl`, `neg`) are the `i32` op.
The others extend their operands first, with `i32.extend8_s`/`extend16_s`
or an `i32.and` with a mask: `div`, `rem`, `shr`, comparisons, `convert`
to a wider type, and `case`. A signed `div` traps when the result doesn't
fit, as `i32.div_s` does, and so does `convert` from a float out of range.
Arrays of them are packed, one or two bytes per element, and so are
fields of data types, which store only the low bits. A host sees an `i8` or `i16` as an `i32`,
and should only read its low bits.

### Closures

The code calling an unknown closure only has the closure and the
arguments: it doesn't know what was captured. So for each `Func` used in
a `Closure`, the backend generates a wrapper `f_entry(env, args..)`. It
reads the captures out of the closure object `env` and calls
`f(captures.., args..)`. A closure object holds its wrapper's slot in
the function table, followed by the captured values ("flat"
closures). `apply(c, args..)`
loads the wrapper from `c` and calls it with `c` and `args`. Calls
rewritten by "Known closures" skip the wrapper.

## Effect handlers

The Wasm backend lowers handlers whose clauses are all tail-resumptive or
aborting without capturing a continuation:

- A global holds a stack of handler frames. A frame holds its handler's
  number, the next frame, a reference to each clause, those of each
  effect it handles in turn, and the handler's environment.
- `handle` pushes a frame, applies the body, pops the frame, and calls the
  return clause with the body's results.
- `perform` walks the stack to the nearest frame of a handler of its
  effect, trapping if there's none: it knows which handlers those are,
  and where each keeps the clause. It then calls that frame's clause directly, with the
  stack set to the frames below while the clause runs, so that a
  `perform` in the clause goes to an outer handler, as it would with a
  captured continuation.
- An aborting clause's wrapper stores what the clause gives in the frame
  and throws a Wasm exception holding the frame. A handler with such a
  clause runs its body in a `try_table`, which catches the exception,
  rethrows it if the frame isn't its own, and otherwise gives the frame's
  results.
- A tail-resumptive clause that discontinues sets the stack to that of
  the `perform` it handles, which a second global holds while the clause
  runs, and performs. A `perform` sets that global and puts it back after,
  and a `handle` that catches an abort puts it back too, since the
  `perform`s the exception left couldn't.
- Each export runs under a wrapper that empties both globals and puts
  them back after, so that an export the host calls from inside an
  extern starts with no handlers.

A handler with any other clause needs its continuations captured. There
are two ways to do that, both possible because handlers are confined to
three ops.

- **Wasm stack switching** (the typed-continuations proposal), which the
  Wasm backend does when `Options::stack_switching` asks for it. Each
  operation of an effect such a handler handles has a control tag. A
  handler that shares a clause with such a handler switches stacks too,
  since a clause is lowered one way.
  `handle` makes a frame with -1 in its clause slots and calls a function
  that links it onto the handler stack and runs the body as a Wasm
  continuation, with a `resume` that has a label per clause. `perform`
  finds the nearest frame of a handler of its effect as for any handler, and if its
  slot is -1, `suspend`s with the operation's tag, putting its handler
  stack back once it's resumed. The `resume` then calls the clause,
  under the handlers outside, with a Conduit continuation: a closure that
  resumes the Wasm continuation under the handler again, which makes the
  handler deep. Memory can't hold a Wasm continuation, which is a
  reference, so it goes in a table of its type, and the Conduit
  continuation holds its slot. The frame's link to the handlers outside is set each time,
  so a continuation resumed somewhere else sees the handlers there. A
  Wasm continuation can only be resumed once, so a second `resume` traps,
  as rule 7 of [ir.md](ir.md) says. `discontinue F.op(k, args)` throws a closure that
  performs `F.op(args)` into `k` with `resume_throw`, and the `perform`
  that `k` was captured at catches it and calls it. Handlers whose clauses
  are all tail-resumptive or aborting keep the lowering above, and only a
  module with a handler of this kind needs the proposal, which isn't
  standard yet: Wasmtime has it on Linux and macOS on x86-64.
- **Evidence passing** (Xie & Leijen, "Generalized evidence passing for
  effect handlers"), which `conduit_effects::lower_effects` does: a
  Conduit-to-Conduit pass over the whole module that removes the four
  ops, rows and continuation types. Functions that may perform, or that
  apply closures, handle or call such functions, take evidence: the
  active handlers and a shared yield state. Tail-resumptive clauses are
  called directly; any other clause is yielded to, and the code after each
  call that may yield, which rows tell, becomes a function that the
  continuation calls (Koka's "yield bubbling"). Each continuation carries
  a "used" flag, so a second resume traps, as with stack switching. The
  handler that's yielded to takes the continuation and the arguments out
  of the yield state, which then holds nothing for that yield, so the
  runtime's own objects never form a cycle; a test checks this on the
  evaluator's heap after every run. The output is ordinary Conduit IR, so every backend runs it, including Wasm
  without stack switching and native ones. See [lowerings.md](lowerings.md).

## Operations per target

- **The shadow stack** is memory that grows down, in segments: the main
  stack starts on a region of `Options::shadow_stack` bytes, 64 KiB by
  default, and with stack switching each continuation on a segment of
  `Options::continuation_stack` bytes, 4 KiB by default. A push that
  doesn't fit chains another segment, from a free list or the heap, or a
  bigger one for what doesn't fit in one, and popping gives the segments
  chained since back to the free list. A continuation keeps its pointer
  and limit in its first segment while it's suspended, and gives its
  segments back when its body returns or throws, or when it's freed
  unused. It holds what an `#[abi(flat)]` extern call passes through
  memory, and objects that don't escape.
- **Objects that don't escape** go on the shadow stack instead of the
  heap, with `Options::stack_objects`, on by default: those a function
  makes with `new`, `array_new`, `array_fill`, `array_alloc`,
  `array_clone`, `ref_new` or `closure` that nothing can still refer to
  once it returns (see `crates/conduit-wasm/src/escape.rs`). An object
  escapes if it's returned, stored in an object, captured, boxed,
  performed or handled, given to an `apply`, to a call in tail position,
  or to a call whose parameter escapes, which a fixpoint over the module
  finds. One made inside a recursive join point stays on the heap, since
  a loop would push one each time around. A function with stack objects
  pops them when it returns and before each tail call, and an aborting
  `handle` pops back to where it started when it catches its abort,
  since the frames the abort unwinds don't. When counting, a stack
  object is counted as any other, but marked buffered, so that its count
  reaching 0 drops what it holds without giving its memory to the heap,
  and `drop_reuse` doesn't reuse it.
- Every object starts with a header word, and every allocation goes
  through one `alloc` function, but a stack object's. By default, `alloc` bumps a pointer and
  memory is never freed. With `Options::counts`, for a module whose counts
  `conduit_opt::ownership::insert_counts` made explicit, the header is the
  count, constants' with the sign bit set so they're never counted, and
  freed objects go on free lists, one per multiple of 8 bytes up to 256
  and one for bigger blocks, which `alloc` takes from before it bumps.
  Each kind of object (a data type, arrays and refs of what they hold,
  closures, `any`) has a drop function, which frees it at 0 and drops what
  it held, tail-calling the drop of its last field so that dropping a
  list is a loop. A closure's wrapper has a function next to it in the
  table that frees it and drops its captures, and every `any` is then a
  box that holds the kind of what it holds. Handler frames are freed when
  their `handle` is done, also when an abort unwinds through it. A module
  whose handlers need evidence passing must have it done before its
  counts are inserted. With stack switching, they needn't be: a captured
  continuation is counted, holds a reference to the handler's
  environment, and gives back its table slot when it's used, or its slot,
  the handler's frame and its shadow stack segment when it's freed
  unused. The module exports `conduit_live`, the number of
  objects not yet freed.
- **Host values** are boxes of 16 bytes, counted like other objects, that
  hold the number of a slot in a table of `externref`s, and a null
  `nullable host` is the address 0. Extern calls and exports that take or
  give them convert at the boundary, and a box whose count gets to 0
  clears its slot and goes on a free list. A module with `host_function`
  imports `conduit.function` from the runtime, has a trampoline for each
  signature of closure, and exports `conduit_release`. All of it is in
  [`host/js.md`](host/js.md).
- With `Options::cycles` too, cycles are collected by trial deletion.
  The header's count is then 28 bits, with the collector's color and a
  "buffered" bit above it, and the sign bit for immortal. A `drop` that
  leaves an object above 0 buffers it if its kind can be in a cycle,
  which the kinds' graph decides, leaving out fields typed `imm`. Each
  kind has a second drop for objects typed `imm`, which never buffers.
  A full buffer of 256 is collected, and so is what the export
  `conduit_collect` asks for. A buffered object's memory only ever goes
  back through the collector, so the buffer never points at another
  object. The collector follows what objects hold with a stack in
  memory, which grows as needed, so it doesn't recurse.
- Handlers whose clauses are all tail-resumptive or aborting are lowered
  directly. For a module with any other clause, the consumer chooses with
  `Options::stack_switching`: stack switching, which isn't standard yet,
  or evidence passing first, which runs on any engine.

Status:

- ✅ supported directly
- 🔧 supported through a Conduit pass
- ⚠️ needs a Wasm proposal that isn't standard yet

| Operation | Wasm | Status |
|---|---|---|
| `Prim`: integer arithmetic, bitwise ops | `i32.add`, `i64.and`, ... | ✅ |
| `Prim`: float arithmetic | `f64.add`, `f32.sqrt`, ... | ✅ |
| `Prim`: comparisons | `i32.lt_s`, `f64.eq`, ... | ✅ |
| `Prim`: conversions | `i64.extend_i32_s`, `f64.convert_i32_s`, ... | ✅ |
| `Call` | `call` | ✅ |
| `Call` in tail position | `return_call` | ✅ (tail calls, Wasm 3.0) |
| `CallExtern` | `call` of an import | ✅ |
| `CallExtern` with `host` values | `call`, with `table.get` of each host argument's slot and a box for each host result | ✅ |
| `CallExtern` of an `#[abi(flat)]` extern | `call`, with addresses and lengths for arrays, an iovec list and out-parameters on the shadow stack | ✅ |
| `Closure` | allocate, store the wrapper's table slot and the captures | ✅ |
| `Apply` | `call_indirect` | ✅ |
| `New` | allocate, store the constructor's index and the fields | ✅ |
| `Get` | `*.load`, or `load8_s`/`load16_s` for `i8` and `i16` | ✅ |
| `Set` | `*.store` | ✅ |
| `ArrayNew` | allocate, store the length and the elements | ✅ |
| `ArrayGet` | bounds check, then `*.load` | ✅ |
| `ArraySet` | bounds check, then `*.store` | ✅ |
| `ArrayLen` | `i32.load` of the length | ✅ |
| `ArrayAlloc` | allocate, which is zeroed | ✅ |
| `ArrayCopy` | bounds checks, then `memory.copy` | ✅ |
| `ArrayClone` | allocate, then `memory.copy` | ✅ |
| `ArrayFreeze` | nothing | ✅ |
| `RefNew` | allocate one slot and store, or a local if promoted | ✅ |
| `RefGet`, `RefSet` | `*.load`, `*.store` | ✅ |
| `Box` of a scalar | allocate and store | ✅ |
| `Box` of a reference | nothing: it's already an address; a box, as for a scalar, with `Options::counts` | ✅ |
| `Unbox` to a scalar | `*.load` | ✅ |
| `Unbox` to a reference | nothing | ✅ |
| `Const` | the address of its object in a data segment, or its literal | ✅ |
| `Undefined` | 0 | ✅ |
| `Forget` | nothing | ✅ |
| `Dup`, `DecRef` | a call that changes the header, unless the object is 0 or immortal | ✅ with `Options::counts`, else nothing |
| `Drop` | a call of the drop function of the object's kind | ✅ with `Options::counts`, else nothing |
| `IsUnique` | whether the header is 1 | ✅ with `Options::counts`, else 0 |
| `Free` | a call that puts a data object or array on its free list | ✅ with `Options::counts`, else nothing |
| `DropReuse` | a call that drops what the object holds and gives its address if it was unique, else 0 | ✅ with `Options::counts`, else 0 |
| `ReuseToken` | the object's address, or 0 if it's buffered, which it then frees for the collector | ✅ with `Options::counts`, else 0 |
| `Reuse` | `New` in the token's memory, or allocated if it's 0 | ✅ |
| `Handle`, `Perform` with tail-resumptive clauses | a handler stack in a global, and a `call_indirect` of the clause | ✅ |
| `Handle`, `Perform` with aborting clauses | the same, then `throw`, caught by `try_table` | ✅ (exception handling, Wasm 3.0) |
| `Discontinue` in a tail-resumptive clause | a `perform` under the handler stack of the `perform` the clause handles, kept in a second global | ✅ |
| `Handle` with other clauses | `cont.new` + `resume` with `(on $op ..)` | ⚠️ stack switching, with `Options::stack_switching` |
| `Perform` to such a handler | `suspend` | ⚠️ stack switching |
| `Resume` | a `call_indirect` of the continuation, which `resume`s | ⚠️ stack switching |
| `Discontinue` | `resume_throw` of a closure that performs the operation, which the `suspend` catches and calls | ⚠️ stack switching |
| `Handle`, `Perform`, `Resume`, `Discontinue` | evidence passing, then the rows above | 🔧 without `Options::stack_switching` |

Notes:

- `case` on an ADT loads the constructor's index, at offset 4.
- A constant is laid out when the module is built, in a data segment
  from address 16, and the heap starts after the constants. 0 is never an
  object, so `undefined` is 0, which is also a reuse token that holds no
  memory. A constant closure is its entry's slot in the table at 4; a
  constant box is laid out as `box` makes one, its kind 0, as an immortal
  box never drops what it holds.
- Integer division and remainder trap on zero in Wasm, so `div` and `rem`
  check the divisor first and give 0 or the dividend, unless it's a
  literal.
- Mutability doesn't exist at run time: `Imm` and `Mut` objects and
  closures have the same layout.
