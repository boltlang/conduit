# Optimizations

`conduit-opt` rewrites one function at a time, reaching other items only
through a `Program`, so a frontend can optimize each function on its own
and cache the result. Every pass takes verified IR and gives verified IR,
and gives the same output for the same input.

`optimize_func` runs the passes below in this order, then again, until a
round changes nothing (at most 16 rounds). Each pass leaves work for the
others, which the next round picks up. At the end, variables and labels
are renumbered so that no gaps are left.

The test programs in `tests/programs/<pass>` show each one, with the code
before and after. Lowering effects and inserting counts aren't
optimizations, though they run after them: they're in
[lowerings.md](lowerings.md).

## A few rewrites

Because the IR is so small, the classic simplifications are local
rewrites, which the passes below are made of:

- **Case of known constructor or literal.** If `case x` has a scrutinee
  bound by `New(C)` or a literal, replace the case with the matching arm.
  A constructor never changes, so this holds for `Mut` objects too.
  `Get(C, i)` of a known `New` of an `Imm` object becomes its argument.
- **Constant folding.** A `Prim` op on literals is its result, as the
  evaluator computes it, unless it traps or a float in it is NaN, whose
  bits Wasm doesn't fix. Integer ops like `add(x, 0)` and `mul(x, 1)` are
  `x`, and `mul(x, 0)` is `0`. Floats get no such rules: `x + 0.0` isn't
  `x` when `x` is `-0.0`.

  The `simplify` pass does these two in one forward walk, since each gives
  the other more to do. It runs after `promote`, which turns what refs
  held into values it can see.
- **Case-of-case.** In A-normal form with join points, this becomes "a jump
  to a known join point with a known argument". Inline (copy) the join
  body at that jump, substituting the arguments, and the case inside it
  becomes a case of a known value. Case-of-case falls out of inlining join
  points plus the previous rule.

  The `joins` pass does this, right before `simplify`. A join definition
  that only one jump goes to is moved there. A small one (at most 20
  `let`s and tails) is copied into a jump that gives a literal, or an
  object made by `new` or `const`, for a parameter it `case`s on. This is
  what inlining a function that returns a `new` object needs: the rest of
  the caller is the join point the result is jumped to. A copy gets fresh
  variables and labels for what it binds. Recursive definitions stay,
  since a copy would unroll the loop, and so do definitions that mention a
  continuation, whose uses the verifier counts along each path.
- **Inlining functions.** Substitute the body and rename `ret` to a fresh
  join point that holds the rest of the caller. This is the same shape as
  any other join point.

  The `inline` pass does this, right after known closures. After a tail call,
  or any call whose results only go on to a jump, the callee's `ret` is
  that jump's target, so no join point is made and the callee's tail calls
  stay tail calls. It inlines callees of at most 20 `let`s and tails, and
  stops once the caller has 1000. It never inlines a function into itself,
  nor one that calls itself directly or through one other function, which
  would unroll it a level each run. Nor a callee with mutability
  parameters that boxes: `box` at a type with a parameter gives `mut any`,
  and at `imm` gives `imm any`, which doesn't fit the variable it binds.
  Inlined code keeps its sources, or gets the call's. The callee is reached
  through `Program::func`, so a Salsa frontend records the dependency.
- **Known closures.** When `apply(c, args..)` has a `c` bound by
  `closure f(captures..)`, replace it with `call f(captures.., args..)`.
  Because of the parameter order (captures first), this is just putting two
  argument lists together. The result is a plain `Call`, so inlining
  applies to it, and if `c` has no uses left, dead-code elimination removes
  the allocation. The `closures` pass does this, first among the passes,
  so that inlining sees the calls it makes in the same round. Not for a
  closure that captures a continuation: the verifier leaves its uses
  through the closure to run time, and two calls passing it would be two
  uses it can see.
- **Common subexpressions.** A `Let` with the same op and arguments as
  one on every path before it gives the same result, if the op only
  computes, reads what can't change (an `Imm` object or array, an array's
  length, a box), or reads what can change with nothing in between that
  may write. An op that may trap can be shared too, since the second
  can't trap if the first didn't. An op that makes an object is shared
  only if nothing can tell the two apart: an `Imm` object or array, a
  closure, a box. A join point can be jumped to after anything, so reads
  of what can change aren't carried into one. The `cse` pass does this,
  after `simplify`, since folding makes more ops look alike.
- **Dead code.** Drop a `Let` whose results are unused and whose
  `op.effects()` doesn't write or trap, and drop a `Join` whose labels are
  unused.
- **Promote refs** (like LLVM's `mem2reg`). A ref that doesn't escape is
  one whose only uses are as the first argument of `RefGet` and `RefSet`
  in the function that creates it. It isn't captured by `Closure`, passed
  to a call or a jump, stored with `New`/`Set`/`ArraySet`/`RefSet`, or
  returned. Promote it like this: every join point it's live into gets an
  extra parameter for its current value, every `Jump` passes the current
  value, `RefGet` becomes that value, and `RefSet` makes a new value
  current. A ref that does escape stays a heap cell.

  The `promote` pass does this. A join point gets a parameter only for the
  refs it needs: those that some path from its start reads before writing,
  found by a backward liveness analysis repeated over the join points until
  it stops changing, so loops and join points that jump to each other work.
  Promoting one ref can let another be promoted on the next round, like a
  ref that was only stored in a promoted one. A ref of a continuation
  stays, since the verifier counts a continuation's uses along paths and
  reads out of a ref on different paths would look like several uses. It
  runs before `dce` and `copies`: a ref holding an array ties the array to
  the ref's region, so promoting it lets `copies` freeze the array.
- **Remove copies.** The `copies` pass does three things:
  - An `ArrayClone` of an `Imm` array to `Imm` is the array itself, since
    nothing can tell the two apart.
  - A `New` of an `Imm` object rebuilt from the fields of another `Imm`
    object `x` of the same type, `new C(get C.0(x), .., get C.n(x))`, is
    `x`. Nothing compares objects by identity, so an `Imm` object can't be
    told apart from another with the same fields. This is what a
    frontend's copy to `Imm` comes down to on a value that's `Imm`
    already, once it's inlined.
  - **Freeze in place.** If nothing else can reach the original array and
    it isn't used afterwards, an `ArrayClone` to `Imm` becomes an `ArrayFreeze`.
    The pass shows this with a region analysis: variables that may reach
    the same `Mut` object are in the same region, because an op stores one
    in the other, reads one out of the other, captures or boxes it, or
    passes it to a join point. Values whose type is `Imm` all the way down
    are left out, since what they reach never changes. A region escapes if
    a parameter, a call's argument or result, or a returned value is in
    it. A clone is frozen if its argument's region doesn't escape and none
    of its variables is live after the clone, with liveness following
    jumps, so a loop that uses the original on its next round keeps the
    clone. The analysis follows what each closure captures through
    `Closure`'s arguments, not through the closure's type.
- **`Imm` closures don't write.** An `Apply` on an `Fn(_, Imm)` whose
  arguments are all `Imm` all the way down can only reach `Imm` objects
  and the ones it makes, so it writes nothing that existed before, unless
  it performs: a clause can write whatever its handler's environment
  holds. With effect types, an empty row on the closure says it can't, and
  then its memory effect is `Reads`. It may still trap or not come back,
  so it can't be removed, but a read before it can be reused after it,
  which `cse` does.

Several of these replace a tail with a block, like a `case` with its arm or
a jump with the join point's body: the block's `let`s then go after the
enclosing block's, and its tail becomes the new tail.

A pass is usually a loop over a block's `let`s and a match with 4 arms on
its tail, recursing only into nested blocks. Most passes only do real work
in one or two places.

## Handlers their body can't use (`handlers`)

A `handle H(env.., c)` whose body `c` performs nothing, by its row, never
runs a clause, so it's `apply(c)` and then a call of `H`'s return clause.
Known closures and inlining then make it direct. A body that performs
some other effect keeps its handler, even when its row lacks `H`'s effect:
an outer clause for that other effect may `discontinue` with `H`'s effect,
which then performs it inside the body, under `H`.

## Known closures (`closures`)

`apply(c, args..)` where `c` is `closure f(captures..)` in the same
function becomes `call f(captures.., args..)`. Lifted functions take their
captures first, so this only puts two argument lists together. The call
skips the closure's wrapper and can be inlined. If nothing else uses the
closure, dead code removes it. A closure that captures a continuation
stays applied: the verifier counts a continuation's uses on each path but
leaves those through a closure to run time, so turning two applies into
two calls that pass it would give code it rejects.

## Inlining (`inline`)

`let rs = call g(args)` becomes the body of `g`, with `g`'s parameters
replaced by the arguments and fresh variables and labels for the rest.
`g`'s `ret` becomes a join point holding the rest of the caller, or, after
a tail call, the label the results already go to, so tail calls stay tail
calls. Only callees of at most 20 `let`s and tails are inlined, and only
while the caller is below 1000. A function is never inlined into itself,
nor one that calls itself directly or through one other function.

## Handlers run directly (`direct`)

Right after inlining. A `let rs = handle H(env.., c)` whose body `c` is a
known closure of `f`, which performs `H`'s effect only itself, with every
other op's row empty, and whose clauses are all tail-resumptive without
`discontinue`, or aborting, becomes `f`'s body, copied as `inline` copies
a callee, in place of the `handle`. Its `ret` becomes a join point that
calls the return clause. Each `perform` in it becomes a copy of its clause
without the continuation: a tail-resumptive clause's `ret`, where it
resumes, is a join point holding what followed the `perform`, and an
aborting clause's `ret` is what follows the `handle`, skipping the rest
and the return clause, as an exception does. The copies run where the
`handle` was, under the handlers outside it, as clauses do.

The other passes then see ordinary code: the generator from `ir.md`
folds to its sum, a state handler around a loop becomes a loop variable,
and an exception a jump (`tests/programs/direct`). A body that performs
through a callee that isn't inlined, or that performs another effect
too, keeps its handler, as does a clause that discontinues or does more
after resuming. The body and the clauses must be small, as `inline`'s
callees, and the handler can't have effect parameters.

## Scalar replacement (`sroa`)

Like LLVM's SROA. A `mut` object made by `new` whose only uses are `get`
and `set` of its fields becomes a ref per field, and a `mut` array made by
`array_new` whose only uses are `array_get` and `array_set` at literal
indices in bounds becomes a ref per element. `promote`, next, turns those
refs into values, so nothing is allocated: a `mut` pair updated in a loop
becomes two loop parameters. Any other use keeps the object, like passing
it to a call; a `case` on it or its `array_len` goes first, in `simplify`.
The refs are named after the object, like `s_0`.

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
- **Arms that can't be taken.** An arm that's only `unreachable` is never
  taken, so a `case` with one other arm is that arm, and one with none is
  `unreachable`, unless the arm reads a field of what the `case` is on.
  `trap` is reached, so its arm stays.
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
may write. An `apply` of an `imm` closure without effects, to arguments
that are `imm` all the way down, doesn't count as a write: it can only
reach `imm` objects and the ones it makes. Nor does a call of a function
that only reads: one with no effects, whose ops and callees at most read.
Two such calls with the same arguments are one, if what they give is
`imm` all the way down; two calls that make a `mut` object make two. The
call is still never removed, since it may not come back. A continuation
is never reused,
since the verifier counts its uses along each path.

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

- Handlers whose clauses are called directly when the `perform` isn't in
  the body's own function. The body of a `handle` is always a closure, so
  a `perform` is never in the same function as its `handle`: `direct`
  handles the case where inlining makes the body's function perform
  itself. Otherwise, calling the clause directly needs a copy of the
  body's function, and of what it calls, specialized to the handler. The
  passes rewrite one function at a time and can't add functions, so this
  needs a pass over the whole module.
- Freezing data objects in place, as `copies` does for arrays (#9).
- Narrowing variables' types after inlining, so that `copies` and the
  rebuild rule see `imm` where a callee's parameter was `ro` (#22).
- Lowering a handler directly while it shares a clause with one that
  switches stacks (#29).
- Keeping host values that are never stored in Wasm locals, without a slot
  in the table (#35).
