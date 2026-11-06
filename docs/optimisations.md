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
closure, dead code removes it.

## Inlining (`inline`)

`let rs = call g(args)` becomes the body of `g`, with `g`'s parameters
replaced by the arguments and fresh variables and labels for the rest.
`g`'s `ret` becomes a join point holding the rest of the caller, or, after
a tail call, the label the results already go to, so tail calls stay tail
calls. Only callees of at most 20 `let`s and tails are inlined, and only
while the caller is below 1000. A function is never inlined into itself,
nor one that calls itself directly or through one other function.

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

## Evidence passing (`effects::lower_effects`)

Not one of the passes above, and not run by `optimize_func`: a module-level
pass that frontends and backends run when they want effect handlers
removed, so that a backend without them can run the program. It follows
Koka (Xie and Leijen, "Generalized evidence passing for effect
handlers"). Afterwards the module has no `handle`, `perform`, `resume` or
`discontinue`, no continuation types and no rows, and the passes above
can run on it.

- **Evidence.** A function that may perform, or that applies a closure,
  handles, resumes, or calls such a function, takes an evidence value as
  its last parameter: the active handlers, innermost first, and a shared
  state saying whether code is yielding, and to which handler. Every
  closure takes it too, so that a pure closure can go where an effectful
  one is expected. Each such function keeps its ID as a wrapper with its
  old signature, which makes the evidence and calls the new version.
- **`handle`** makes a node for its handler: the effect, a fresh marker,
  whether each clause is tail-resumptive, a closure for each that is, and
  the evidence outside. It then calls the handler's `drive` function,
  which runs the body and then calls the return clause, or the clause the
  body yielded to, with a continuation, or wraps the continuation of a
  yield to a handler further out so that resuming it re-enters this one.
- **`perform`** finds the nearest node of its effect. A tail-resumptive
  clause is called directly, under the handlers outside it. Any other is
  yielded to: `perform` records the operation, its arguments and the rest
  of its function, and returns `undefined` results.
- **Yielding.** After each call that may yield, the code checks the state
  and, if it's yielding, adds the rest of its function to the
  continuation, as a new function, and returns. Join points in scope
  there become functions first, since the rest jumps to them. An effect
  may yield if one of its handlers has a clause that isn't
  tail-resumptive, or performs an effect that may: rows say which calls
  can, so code that only performs effects that don't yield pays nothing
  for it.
- **Continuations** are closures given a closure that gives what the
  `perform` gives: `resume` gives one returning the values, and
  `discontinue` one that performs its operation at the `perform`, under
  the handlers there. A flag makes a second use trap. A node's link to the
  evidence outside is set each time it runs, so a continuation resumed
  somewhere else runs under the handlers there.

The test programs run after it in the evaluator, after the optimizer, and
in Wasm without stack switching.

## Reference counts (`ownership::insert_counts`)

Not one of the passes above either: a module-level pass, run once after
optimization, that adds the `dup`s and `drop`s that free memory (see
"Reference counts" in `ir.md`). Every variable that holds an object owns
a reference to it, parameters of functions and join points included,
except what's borrowed (below):

- A use that takes the reference, while the variable is still needed
  after it or is taken twice by the same op, gets a `dup` first.
- The last use that only borrows it gets a `drop` after, and so does a
  variable whose value is never used. A branch drops what's owned when it
  starts but it doesn't use.
- What `get`, `array_get`, `ref_get` and `unbox` read out of an object is
  `dup`ped right away, so that it's owned, before the object may be
  dropped.
- A join point owns what its body uses from outside, which each jump to
  it hands over with its arguments.
- What `const` and `undefined` give is never counted.

**Borrowed parameters** (`ownership::borrows`, which `insert_counts`
calls; a frontend can give its own with `insert_counts_with`). An object
parameter is borrowed unless the function gives its reference away: to a
use that takes it, like a field, a jump, a return or an owned parameter
of a call. Then the caller keeps the reference, the function never drops
it, and it `dup`s it for each use that takes one. What's read out of a
borrowed `imm` object is borrowed too, since the object keeps it alive and
unchanged for the whole call: a function that walks a list it borrows
counts nothing. It's a fixpoint over the module, as Lean's is, with two
rules that keep parameters owned:

- A function made into a closure, or a handler's clause, owns all its
  parameters, since `apply` and `perform` always give theirs away.
- A tail call between functions that can call each other makes the
  callee's parameter owned if the caller owns what it passes, so that the
  caller needn't drop it after the call, which would grow the stack of a
  loop written as tail calls. Other tail calls just lose being tail calls.

`crates/conduit-opt/tests/counts.rs` runs
every test program after it, as written, after optimizing, and after
evidence passing, with the evaluator counting, and again after
specializing drops: every count must be right.

**Specialized drops** (`ownership::specialize_drops`, after
`insert_counts`). A `drop(x)` of an `imm` data object, in a block where
fields read from `x` were `dup`ped before it, becomes a branch on
`is_unique(x)`. If `x` is unique, its references to those fields become
the variables', so the `dup`s go, the other object fields are dropped,
and `x` is freed with `free`. If not, the `dup`s stay and `decref(x)`
takes 1 from its count. The rest of the block becomes a join point that
both branches jump to. Only `imm` objects, whose fields can't have
changed since they were read, and only where a `dup` can go: a `drop`
with nothing to cancel is already as fast as its expansion.

## Not done yet

- Handlers whose clauses are called directly. The body of a `handle` is
  always a closure, so a `perform` is never in the same function as its
  `handle`: calling the clause directly needs a copy of the body's
  function, and of what it calls, specialized to the handler. The passes
  rewrite one function at a time and can't add functions, so this needs a
  pass over the whole module.
