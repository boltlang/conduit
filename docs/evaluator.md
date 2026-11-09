# The evaluator

`conduit-eval` runs Conduit IR directly, for testing frontends and passes.

- **It only accepts verified IR.** It doesn't check types while running.
  IR the verifier would reject gives an "invalid IR" error or a wrong
  result, but never a crash.
- **Control flow doesn't use Rust's stack.** The evaluator runs a loop over
  an explicit stack of frames. A frame holds a place in a block: `let`s
  move it forward, and jumps, loops and `case` move it to another block.
  Calls push a frame, and tail calls and tail resumes reuse the
  caller's frame. A deeply recursive program hits a frame limit, which is
  an error.
- **Handlers move frames.** `handle` pushes a handler frame. `perform`
  moves the frames from the nearest handler of its effect up into the
  continuation, and `resume` moves them back. `discontinue` moves them
  back too, and then performs in the frame that was waiting. A second use
  finds the frames gone, which is an error (rule 7 of "Effect handlers" in [ir.md](ir.md)).
- **Host values** are objects too, counted and checked for leaks like
  others. A host function sees one as `Value::Host(n)`, the number it gave,
  a null as `Value::Null`, and a host function as `Value::Function(k)`,
  which it calls later with `Machine::call_function` (see "The evaluator"
  in [`host/js.md`](host/js.md)).
- **Objects know their mutability.** Every object that can be `Imm` or
  `Mut` carries an "is mut" flag. A write to an `Imm` object is an error,
  which can only happen if an optimization broke the mutability rules.
  `ArrayFreeze` clears it, so a write through an alias that a wrong `ArrayFreeze`
  left behind is an error too. An element `ArrayAlloc` made is unset
  until something sets it, and reading it is an error too. A frame and a closure carry their
  function's mutability arguments, so that an object made at a parameter's
  mutability gets the caller's, and `Box` and `Unbox` compare types with
  the arguments in place. `Machine::call_at` calls a function with
  mutability parameters.
- **Numbers follow Wasm.** Integers wrap, shift amounts are taken modulo
  the width, and `min` and `max` give NaN if either operand is NaN and
  treat `-0.0` as less than `0.0`. Division by zero gives 0 and its
  remainder the dividend. Division of the smallest integer by -1, and
  converting a float that is NaN or out of range to an integer, are
  errors, where Wasm traps. The test
  programs in `tests/programs/prims` check that the two agree.
- **Undefined behavior is an error,** like `Unbox` at the wrong type.
- **Constants are made when first used,** and kept, so every use gets the
  same object.
- **Hosts take and give values,** whatever the extern's ABI. A `Host`
  gets an `Arrays` view of the heap, which reads any array it's given and
  writes the `mut` ones, as a flat-ABI host does through memory.
- **Reference counts are checked** with `Machine::counting` on. Every
  object has a count, which `Dup`, `Drop` and the ops that count for
  themselves change (see "Reference counts" in [ir.md](ir.md)), and an object is freed at
  0. A use of a freed object, or a `Drop` of one, is undefined behavior,
  which stops the machine, and so is counting what `undefined` gave:
  its `Dup` or `Drop`, freeing an object that holds it, or a `Set` or
  `RefSet` over it. `Forget` changes no count. After a run, `Machine::leaks` takes the values
  that still own objects, like the results, and reports objects with more
  counts than references (a missing `Drop`), with fewer (a `Drop` too
  many), references to freed objects, and garbage that only cycles keep
  alive. With `Machine::cycles` on too, it collects cycles by trial
  deletion (Bacon and Rajan, synchronous): an object that may be in a
  cycle, which only a `Mut` object, a ref or an `any` can, is buffered when
  a `drop` leaves it above 0, and the buffer is collected once it's full
  or by `Machine::collect_cycles`. Tests run counted programs that way,
  with a small buffer, so that collections happen in the middle of runs.
  A test program with `//# counted` runs that way and must leave
  nothing. Without counting, `Dup` and `Drop` do nothing, and objects are
  never freed.
