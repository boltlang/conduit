# Optimizations

`conduit-opt` rewrites one function at a time, reaching other items only
through a `Program`, so a frontend can optimize each function on its own
and cache the result. Every pass takes verified IR and gives verified IR,
and gives the same output for the same input.

`optimize_func` runs the passes below in this order, then again, until a
round changes nothing (at most 16 rounds). Each pass leaves work for the
others, which the next round picks up. At the end, variables and labels
are renumbered so that no gaps are left.

The design behind them is in "Optimizations come down to a few rewrites"
in [the IR](ir.md). The test programs in `tests/programs/<pass>` show
each one, with the code before and after.

## Known closures (`closures`)

`apply(c, args..)` where `c` is `closure f(captures..)` in the same
function becomes `call f(captures.., args..)`. Lifted functions take their
captures first, so this only puts two argument lists together. The call
skips the closure's wrapper and can be inlined. If nothing else uses the
closure, dead code removes it.

## Inlining (`inline`)

`let rs = call g(args)` becomes the body of `g`, with `g`'s parameters
replaced by the arguments and fresh variables and labels for the rest.
`g`'s `ret` becomes a join point holding the rest of the caller, or, after
a tail call, the label the results already go to, so tail calls stay tail
calls. Only callees of at most 20 `let`s and tails are inlined, and only
while the caller is below 1000. A function is never inlined into itself,
nor one that calls itself directly or through one other function.

## Ref promotion (`promote`)

Like LLVM's `mem2reg`. Frontends turn mutable locals into refs; a ref
that only `ref_get` and `ref_set` use, in the function that made it,
becomes the values it holds. Join points get a parameter for each ref
whose value they need, found by a liveness analysis over the join points.
A ref of a continuation stays.

## Join point inlining (`joins`)

How case-of-case comes about. A join definition that only one jump goes
to is moved there, with its parameters replaced by the arguments. A small
one (at most 20 `let`s and tails) is copied into a jump that gives a
literal, or an object made by `new` or `const`, for a parameter it `case`s
on, and `simplify` then picks the arm. This is what inlining a function
that returns a `new` object needs: the rest of the caller is the join
point the result is jumped to. Recursive join points stay, since a copy
would unroll the loop, and so do ones that mention a continuation.

## Simplification (`simplify`)

Rewrites from what a variable is known to be, in one forward walk:

- **Constant folding.** A scalar op on literals is its result, unless it
  traps or a float in it is NaN. `add(x, 0)`, `mul(x, 1)` and the like are
  `x`, and `mul(x, 0)` is `0`. Floats get no such rules: `x + 0.0` isn't `x`
  when `x` is `-0.0`.
- **Case of a known value.** A `case` on a literal, an object made by
  `new` or a constant takes its arm.
- **What an arm says.** In an arm `C =>` of a `case` on `x`, another
  `case` on `x` takes `C`, and in an arm `3 =>`, `x` is `3`.
- **Fields of a known object.** A `get` of an `imm` object made by `new`
  is the `new`'s argument.
- **Parts of a constant.** A constant that's a literal is the literal. A
  field, the length and an element at a literal index in bounds are
  known: a literal, or `const` of the constant that part is.
- **Arrays made here.** The length of an array made by `array_new`,
  `array_fill` or `array_alloc`, or cloned or frozen from one, is known.
  An element of an `imm` one at a literal index in bounds is what it was
  made with.
- **Unboxing what was boxed.** `unbox[T](box[T](x))` is `x`.

## Common subexpressions (`cse`)

A `let` with the same op and arguments as one before it on every path
gives the same result, if the op only computes, reads what can't change
(an `imm` object or array, an array's length, a box, a constant), or reads
what can change with nothing in between that may write. An op that may
trap can be shared too: the second can't trap if the first didn't. An op
that makes an object is shared only when nothing can tell the two apart:
an `imm` object or array, a closure, a box.

A read of what was just written gives what was written: `get C.i(x)`
after `set C.i(x, v)` is `v`, and likewise for `array_set` and `ref_set`,
and for a field of a `mut` object right after its `new`, an element of a
`mut` array right after its `array_new`, and a ref right after its
`ref_new`. Like any read of what can change, this lasts until something
may write. A continuation is never reused, since the verifier counts its
uses along each path.

## Dead code (`dce`)

Drops each `let` whose results are unused and whose op neither writes nor
traps, and each join definition nothing live jumps to. It works bottom-up,
so code only dead code uses goes too.

## Copies (`copies`)

- **Clones that change nothing.** An `array_clone` of an `imm` array to
  `imm` is the array itself.
- **Rebuilds that change nothing.** `new C(get C.0(x), .., get C.n(x))` of
  an `imm` `x`, to `imm` at the same type, is `x`.
- **Freeze in place.** An `array_clone` to `imm` becomes `array_freeze`
  when nothing else can reach the original any more. A region analysis
  shows this: the original's region doesn't escape the function, and none
  of its variables is live after the clone.

## Not done yet

- `imm` closures with no effects don't write, so `cse` could reuse reads
  across an `apply` of one.
- Scalar replacement: a `mut` object or small array that doesn't escape
  becomes its fields, as `promote` does for refs.
- Calls of functions known to only read, which `cse` could share.
- Handlers: a `perform` whose `handle` is in the same function could call
  the clause directly.
