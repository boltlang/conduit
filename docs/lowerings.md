# Lowerings

Two Conduit-to-Conduit passes that aren't optimizations, and that a
frontend or backend runs after optimizing, when it wants what they give:
evidence passing, which removes effect handlers so that a backend without
them can run the program, and reference counting, which makes the counts
explicit so that memory is freed. [frontends.md](frontends.md) says in
which order they run.

## Evidence passing (`conduit_effects::lower_effects`)

Not one of the passes above, and not run by `optimize_func`: a lowering
of the whole module, in its own crate, `conduit-effects`, which frontends
and backends run when they want effect handlers removed, so that a
backend without them can run the program. It follows
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
- **`handle`** makes a node for its handler: a constant table, for each
  effect, of where its clauses start among the handler's, or -1 for one
  it doesn't handle; a fresh marker, whether each clause is tail-resumptive, a closure for each that is, and
  the evidence outside. It then calls the handler's `drive` function,
  which runs the body and then calls the return clause, or the clause the
  body yielded to, with a continuation, or wraps the continuation of a
  yield to a handler further out so that resuming it re-enters this one.
- **`perform`** finds the nearest node whose table has its effect, and
  the clause there. A tail-resumptive
  clause is called directly, under the handlers outside it. Any other is
  yielded to: `perform` records the operation, its arguments and the rest
  of its function, and returns `undefined` results.
- **Yielding.** After each call that may yield, the code checks the state
  and, if it's yielding, adds the rest of its function to the
  continuation, as a new function, and returns. Join points in scope
  there become functions first, since the rest jumps to them. An effect
  may yield if one of its handlers has a clause for it that isn't
  tail-resumptive, or can perform an effect that may: rows say which calls
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

**Reuse** (`ownership::insert_reuse`, before `insert_counts`). In an arm
`C =>` of a `case` on `x`, where the arm's own block has `x`'s last use,
a `drop_reuse(x)` goes right after it, and the first `new C'` after that,
of a constructor of the same shape, becomes `reuse C'` of its token. If
the block has no such `new`, or uses `x` in its tail, each branch after it
that doesn't use `x` gets the same treatment from its start, so inserting
into a tree, which rebuilds a node in one of the arms of a comparison,
still reuses it. When
`x` turns out to be unique, the new object is built in its memory instead
of freeing one and allocating the other: `map` over a list it owns makes
no new cells. The `drop_reuse` goes right after the last use, not at the
`new`, so that `x` doesn't keep its fields alive meanwhile, and a
recursive call on a field still finds it unique. Only within the arm's
block, so the token is always used, and not where the last use gives `x`
away. It makes `x` owned, since `drop_reuse` takes it.

**Specialized drops** (`ownership::specialize_drops`, after
`insert_counts`). A `drop(x)` of an `imm` data object, in a block where
fields read from `x` were `dup`ped before it, becomes a branch on
`is_unique(x)`. If `x` is unique, its references to those fields become
the variables', so the `dup`s go, the other object fields are dropped,
and `x` is freed with `free`. If not, the `dup`s stay and `decref(x)`
takes 1 from its count. The rest of the block becomes a join point that
both branches jump to. Only `imm` objects, whose fields can't have
changed since they were read, and only where a `dup` can go: a `drop`
with nothing to cancel is already as fast as its expansion. A
`drop_reuse(x)` is specialized the same way, with the rest of the block
taking the token: `reuse_token(x)` when `x` is unique, so that `map`
rebuilds a unique cell with no counts at all, and `undefined`, which is no
memory, after the `decref` when it's shared.

The `dup`s at the end of a block, of fields read from an object that a
branch of the block's `case` drops, move into the start of every branch
first, when nothing after them counts or gives away the field or the
object, so that the `drop` in a branch can cancel them, as in a search
tree's insert. A `dup(y)` that a `drop(y)` then follows with no use of `y`
between goes, with the `drop`.

It also **freezes in place**: an `array_clone` to `imm` that is its
array's last use, followed by the array's `drop`, becomes a branch on
`is_unique`: the array itself, by `array_freeze`, when it's unique, and a
clone and a `decref` otherwise. The `copies` pass can only prove that
statically; this catches the rest at run time.
