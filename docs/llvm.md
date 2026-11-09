# The LLVM backend (experimental)

`conduit-llvm` lowers a module to LLVM IR as text, which `clang` or `llc`
compile, with no LLVM library to link: a first step towards a native
backend, to find the shape of the lowering before moving to the LLVM API
(inkwell, for LLVM 23). It handles everything but handlers, which lowering
effects removes first, and host values, and says so for what it doesn't
handle. Memory isn't freed yet.

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
- Objects: data, arrays, refs, `any` and closures, from `malloc`, laid
  out as in the Wasm backend: a header word for the count, then the
  constructor, length or kind at 4, then the fields from 8, each at a
  multiple of its size. A closure holds its entry function's address at 8,
  then its captures; each function and number of captures has an entry
  function, `@fn<f>_entry<n>(closure, params..)`, which reads the captures
  and tail-calls the function. Array accesses and copies out of bounds,
  and negative lengths, trap.
- Constants, as globals laid out the same way, with the immortal bit set
  in their header.
- Externs with `#[link(c, symbol = "..")]`, called with the C calling
  convention, and exports with `#[export(c, symbol = "..")]`, which get a
  C-callable function.

Not yet:

- **Freeing memory.** Nothing is freed, so `dup` and `drop` do nothing,
  no object is unique and a reuse token holds no memory, as in the Wasm
  backend without counts. Counting comes next: the header's layout is
  already the Wasm backend's.
- **Handlers:** lower effects first. Evidence passing gives ordinary
  data, closures and `any`s, which this backend handles, so the rest of
  the pipeline is the same as for Wasm without stack switching.
- **Host values,** which need a host like JavaScript.

## How

| Conduit | LLVM |
|---|---|
| function `fn1` | `define tailcc .. @fn1(..)` |
| variable | an `alloca` in the entry block, stored to when bound, loaded when used; `mem2reg` makes SSA again |
| `join j(..) = ..` | a block `j<n>`; a jump stores the arguments in the parameters' slots, then `br` |
| `case` | `switch`, on the constructor at 4 for data, with `unreachable` as the default when there's none |
| `apply(c, ..)` | a call of the entry function whose address `c` holds at 8, with `c` first |
| `jump ret(..)` | `ret`, of a struct for several results |
| a call in tail position (see `conduit_ir::tail`) | `tail call tailcc`, then `ret`: `tailcc` makes it a tail call always, as Conduit promises |

## Tests

`crates/conduit-llvm/tests/programs.rs` compiles every test program it can
lower with `clang -O2`, as written and after the optimizer, after lowering
its effects when it has any, and runs its
`run` checks: a `main` of its own takes the check's number, calls the
function, and prints the bits of each result. A trap is a signal. Without
`clang`, it says so and passes.

114 of the 145 test programs run this way. The rest have no `run` checks,
or call the test hosts' `emu` functions or use host values.
