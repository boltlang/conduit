# The LLVM backend (experimental)

`conduit-llvm` lowers a module to LLVM IR as text, which `clang` or `llc`
compile, with no LLVM library to link: a first step towards a native
backend, to find the shape of the lowering before moving to the LLVM API
(inkwell, for LLVM 23). The lowering builds a small model of LLVM IR, in
`conduit_llvm::ir`, which printing makes text. It handles everything but
handlers that capture their continuations, which lowering effects
removes first, and says so for what it doesn't handle. With
`Options::counts`, memory is
freed by reference counting, and with `Options::cycles`, cycles are
collected too.

## What it handles

- Functions of scalars (`i8` to `i64`, `f32`, `f64`), calls, several
  results (as a struct), join points, loops, `case` on integers,
  `unreachable` and `trap`.
- Every prim, with Conduit's semantics where LLVM's differ: division by
  0 gives 0 and the remainder the dividend, signed division that overflows
  traps, `rem.s` of the smallest value by -1 is 0, shift amounts are taken
  modulo the width, `min` and `max` give NaN if either operand is NaN
  (`llvm.minimum` and `llvm.maximum`), and `ne` on floats is unordered.
- Converting a float to an integer traps when it's NaN or out of range,
  as in Wasm: a check against the range's bounds before `fptosi` or
  `fptoui`, which LLVM leaves undefined there.
- Objects: data, arrays, refs, `any` and closures, from the module's
  heap (see "The heap" below), laid out as in the Wasm backend: a header word for the count, then the
  constructor, length or kind at 4, then the fields from 8, each at a
  multiple of its size. A closure holds its entry function's address at 8,
  with counts, the address of its descriptor at 16 (the functions that
  free it and visit what it holds), then its captures; each
  function and number of captures has an entry function,
  `@fn<f>_entry<n>(closure, params..)`, which reads the captures and
  tail-calls the function. An `any` holds its value at 8 and, at 16, the
  descriptor of the kind of what it holds (its drop and visit functions),
  or null. `array_load` and `array_store` are a `load` or `store` with
  `align 1` at the byte offset. Array accesses and copies out of bounds,
  and negative lengths, trap.
- Constants, as globals laid out the same way, with the immortal bit set
  in their header. A constant closure holds its function's entry for no
  captures, and a constant closure's or box's pointer at 16 is null: no
  immortal object is freed.
- `undefined` is left uninitialized: LLVM's `undef`, which it may
  optimize as it likes, since nothing may use it, counting included (see
  "Reference counts" in [ir.md](../ir/ir.md)). An undefined reuse token is
  null, a token that holds no memory. `forget` is nothing.
- Externs with `#[link(c, symbol = "..")]`, called with the C calling
  convention, and exports with `#[export(c, symbol = "..")]`, which get a
  C-callable function. Externs that link to the same symbol share its
  declaration, and must agree on its signature. Of several results, the
  first is returned, and each other one is written through a pointer the
  caller passes after the arguments, as in the flat ABI: C returns a
  struct in ways that depend on its size and the target, which LLVM
  leaves to frontends like clang, and pointers are the same everywhere.
  So `extern f(i32) -> (i32, i64)` is `int32_t f(int32_t, int64_t *)`.
- `#[abi(flat)]` externs, for which the flat ABI is the C ABI: an array
  is a pointer to its elements, 8 bytes into the object, and its length
  as an `i32`, or the pointer alone if it's `#[addr]`; a list of byte
  buffers is a pointer to a `struct iovec`, `{void *base, size_t len}`,
  for each, from `calloc` and freed after the call, and how many there
  are; an `i8` or `i16` is zero-extended to an `i32`, as in Wasm, which C
  takes for a narrower parameter too; and each result after the first is
  a pointer to an `alloca` the callee writes it to.

## The heap

Objects come from free lists by size, in front of `malloc`, as in the
Wasm backend (`src/heap.rs`). Every 8 bytes up to 256 is a size class,
with a free list. `conduit_alloc` takes a block from its class's list, or
else bumps it from a chunk of 1 MiB that `malloc` gives; a bigger block
comes from `malloc` alone. The 8 bytes before each object hold its class,
or 0 for a big one, since `conduit_dealloc`, which every way of freeing
goes through, has only the address: it puts the block back on its list,
linked through its first word, or gives a big one back to `free`. Chunks
are never given back. Without counts, nothing is freed, so every small
object is bumped.

Memory isn't zeroed, since an object's fields are all set when it's made.
`array_alloc` zeroes its elements, which are null until they're set.

## Reference counts

With `Options::counts`, for a module whose counts `insert_counts` made
explicit, objects are counted and freed (`src/counts.rs`):

- The header word is laid out as in the Wasm backend: the count in bits
  0 to 27, the collector's color in bits 28 and 29, whether the object is
  buffered in bit 30, and in bit 31 whether it's immortal, as a constant
  is. A count that would pass `0x0fff_ffff` saturates, as in the Wasm
  backend and the evaluator.
- `drop` goes through a function for each kind of object,
  `conduit_drop.<kind>`, which only checks: it takes one from the count,
  unless the object is null or immortal, and with cycles, buffers what's
  still shared and isn't `imm`. It's `alwaysinline`, so a `drop` of what's
  shared is a decrement where it is, as a `dup` is. Only for the last
  reference does it call `conduit_freeing.<kind>`, `cold` and
  `noinline`, which frees the object and drops what it held, tail-calling
  the drop of its last field, so that dropping a list is a loop. A
  closure's and an `any`'s drop go through the functions they hold.
- `is_unique`, `free`, `decref`, `drop_reuse`, `reuse_token` and `reuse`
  work as in the evaluator, so reuse builds in place. `set`, `array_set`
  and `ref_set` drop what they replace, and `array_fill`, `array_copy` and
  `array_clone` count what they share.
- `@conduit_live` counts the objects allocated and not freed.
- With `Options::cycles` too, cycles are collected by trial deletion
  (Bacon and Rajan, synchronous), as in the Wasm backend. A `drop` or
  `decref` that leaves an object above 0 buffers it, unless its type is
  `imm`: each kind has a second drop for those, which never buffers. The
  buffer holds 256 objects, and is collected when it's full, and when the
  C function `conduit_collect` is called. A buffered object's memory only
  goes back through the collector, so the buffer never points at freed
  memory. Each kind has a visit function, which does a step of the
  collector on what an object holds and pushes it on a stack on the heap,
  so the collector doesn't recurse. The garbage it finds is freed at the
  end, since `free` gives memory back for good: unlike Wasm's free lists,
  a freed header can't be read again.

Without counts, nothing is freed: `dup` and `drop` do nothing, no object
is unique, and a reuse token holds no memory.

## Host values

What a host gives and gets, and who owns what, is in
[../host/native.md](../host/native.md). Inside the module, a host value is
a box of 24 bytes, counted like any other object: the header, then the
host's `value` at 8 and its `ops` at 16. A null `nullable host` is a
null box, which isn't counted. The box holds one of the host's
references, and when its count gets to 0 it calls `ops->release(value)`
and is freed. Counting a `dup` doesn't call the host: it only hears of a
value when it comes in and when it goes. Without counts, nothing is
released, as in Wasm.

`host_function` gives the host a trampoline for each signature of
closure, which runs the closure as an export runs its function (both are
made by `c_entry`), and dups the closure first, since applying it takes
a reference and the host keeps its own. After evidence passing, every
closure takes evidence, so `host_function` gets an adapter's closure, as
in Wasm.

## Handlers

Handlers whose clauses are all tail-resumptive or aborting (see
`conduit_ir::clause`) are lowered without capturing a continuation, as
in the Wasm backend, with LLVM's exception handling for aborts where Wasm
has its own (see `crates/conduit-llvm/src/handlers.rs`):

- `@conduit_handlers` holds a stack of handler frames. A frame is an
  `alloca` in the function that runs the `handle`: its handler's number,
  the next frame, the handler's environment, then, for a handler with an
  aborting clause, its results and an `_Unwind_Exception`.
- `handle` pushes a frame, applies the body, pops it, and calls the
  return clause. For a handler with an aborting clause, the body is an
  `invoke`, whose landing pad only runs when a clause aborts.
- `perform` walks the stack to the nearest frame of a handler with a
  clause for it, trapping if there's none, and calls that clause's
  wrapper, `conduit_clause.<handler>.<slot>`, directly, under the frames
  below, so that what the clause performs goes to the handlers outside.
  The wrapper reads the environment out of the frame, with a reference of
  its own for the clause when counting, and calls the clause as a plain
  function.
- An aborting clause's wrapper stores the clause's results in the frame
  and unwinds with the frame's exception, by `_Unwind_ForcedUnwind`. Each
  `handle` it reaches checks whether the exception is its own frame's,
  and goes on unwinding (`resume`) if not; the one it is gives the
  frame's results. A forced unwind runs landing pads without searching
  for a handler first, so C's personality, `__gcc_personality_v0`, runs
  them, and Conduit needs none of its own. Both come with libgcc_s or
  libunwind, which C programs link on Linux and macOS.
- A tail-resumptive clause that discontinues performs under the handlers
  of the `perform` it handles, which `@conduit_site` holds.
- An export, or a trampoline of `host_function`, runs with no handlers,
  and puts its caller's back when it returns, and, in a module where some
  function has `throw`, also when an exception unwinds it: the call is an
  `invoke` whose landing pad puts them back and resumes.

When counting, an abort drops what the IR says dropping the
continuation would (#56): what the frames it skips own, and the
environments of the handlers it passes and of the one it aborts to.
Each call that may perform, in a function whose row has an effect with
operations, while the frame owns something (see `conduit_ir::owned`), is
an `invoke` whose cleanup landing pad drops that and resumes, and a
`handle`'s landing pad drops its environment. Code that can't perform, or
has nothing to drop, has no landing pads.

**Exceptions from the host,** like a C++ one from an extern, go through
the same landing pads, which `__gcc_personality_v0` runs for any
exception (#59). Where one can come from is `throw` in a row (see
[exceptions.md](../ir/exceptions.md)): each call of an extern
with `throw`, and each call and `apply` that can reach one, is an
`invoke` while the frame owns something. An extern that no extern of its
symbol says may throw is declared `nounwind`, since its throwing would be
undefined behavior.

A `handle` that nothing aborts to costs no more than one of a handler
that doesn't abort, and an abort costs an unwinding, which reads the
unwind tables of each frame it passes: about 0.25µs in
`bench/effects_exceptions`, a few frames down. Against `_setjmp` and
`_longjmp`, which this replaced (#55) and which cost on every `handle`,
`bench/effects_rare_exceptions`, which aborts once every 1024 rounds,
went from 571µs to 431µs (`+specialized`, 100000 rounds), and
`bench/effects_exceptions`, which aborts every 8th round, from 634µs to
3.80ms.

Not yet:

- **Other handlers:** lower effects first. Evidence passing gives
  ordinary data, closures and `any`s, which this backend handles, so the
  rest of the pipeline is the same as for Wasm without stack switching.
  Native stack switching is #46.

## Debug info

The IR's sources, `@src` on `let`s and `jump`s, are opaque to Conduit, so
the frontend says where each one is: a `conduit_llvm::Sources`, of a
`Location` (file, line and column) for each `SourceId`, and the directory
relative files are in, given to `emit_module_with_sources` (#50). With
one, the module has DWARF debug info (`src/debug.rs`):

- each function with a source in its body is a `DISubprogram`, named
  after its debug name, with its symbol as its linkage name, at the line
  of its first source;
- each instruction lowered from a `let` or `jump` with a source has its
  `DILocation`, and one lowered from one without has that of the last one
  before it, so that no call in a function with debug info is without
  one, which LLVM's verifier rejects;
- the backend's own functions, like closures' entries, export wrappers
  and the runtime, have none, and there are no variables yet.

So `gdb`, `lldb` and `perf` show code where it comes from in the
frontend's program. Without sources, the module has no debug info at
all.

## How

| Conduit | LLVM |
|---|---|
| function `fn1` | `define tailcc .. @fn1(..)` |
| variable | an `alloca` in the entry block, stored to when bound, loaded when used; `mem2reg` makes SSA again |
| `join j(..) = ..` | a block `j<n>`; a jump stores the arguments in the parameters' slots, then `br` |
| `case` | `switch`, on the constructor at 4 for data, with `unreachable` as the default when there's none |
| `apply(c, ..)` | a call of the entry function whose address `c` holds at 8, with `c` first |
| `jump ret(..)` | `ret`, of a struct for several results between Conduit's own functions |
| a call in tail position (see `conduit_ir::tail`) | `tail call tailcc`, then `ret`: `tailcc` makes it a tail call always, as Conduit promises |

## Tests

`crates/conduit-llvm/tests/programs.rs` compiles every test program it can
lower with `clang -O2`, built for C (see "Host functions" in
[../../tests/programs/README.md](../../tests/programs/README.md)) and linked with
the test host in `tests/programs/host/c.c`, as written and after the optimizer, after lowering
its effects when its handlers need it, each without counts, with them, and with
cycles collected too, and runs
its
`run` checks: a `main` of its own takes the check's number, calls the
function, and prints the bits of each result. A trap is a signal. Without
`clang`, it says so and passes.

With counts, each run must also leave as many objects live as the
evaluator does, counting the same way: with cycles collected at the end,
when they are, on the program built for the evaluator, not counting its
continuations, which native code doesn't make for the handlers it lowers
itself, and the C host
must hold as many host values as the evaluator has boxes of them. 123
of the 148 test programs run this way: all that have `run` checks.
`tests/host_exports.rs` calls exports that take and give host values from
C, checking each `retain` and `release`, and, from C++, exports whose
externs throw: what the frames it unwinds own is dropped, and a handler
around an extern that calls a throwing export is still there after.
