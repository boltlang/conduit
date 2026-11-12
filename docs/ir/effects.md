# Effect handlers

How Conduit IR expresses effects: handlers and their continuations, and
effect types, the rows that say what code may perform. The rest of the
IR is in [ir.md](ir.md).

Handlers are **deep** (a resumed computation is still handled by the same
handler) and continuations are **one-shot** (each can be
resumed at most once). The IR has mutation, and resuming a continuation
twice would run its writes twice. One-shot continuations are also what
Wasm stack switching provides.

## Declarations

```rust
pub struct EffectDecl {
    ops: Vec<Sig>,         // operation i: its parameters and results
}

pub struct HandlerDecl {
    env: Vec<Ty>,          // values the clauses share, like a closure's
    ops: Vec<(EffectId, Vec<FuncId>)>,
                           // each effect it handles, once, with its clause i
                           // for operation i: fn(env.., op params.., k: Cont) -> R
    ret: FuncId,           // return clause: fn(env.., body results..) -> R
}
```

`R` is the result of the whole `handle`. A handler can handle several
effects, as one OCaml handler's `effc` or one Bolt `match` with effect
arms does, with one return clause. Every operation of an effect it
handles has a clause; a frontend whose `match` leaves some out gives
those a clause that performs the operation again and resumes with what it
gives, which is tail-resumptive and so cheap. The continuation `k` has type
`Cont(op results -> R)`: resuming it with the operation's results runs the
rest of the body, through the return clause, and gives back an `R`.

## The four operations

```
let r = handle H(env.., body)   -- run closure `body` under handler H
let x = perform E.op(args)      -- nearest enclosing handler of E (dynamic)
let r = resume(k, vals)         -- continue a captured computation
let r = discontinue F.op(k, args) -- continue it by performing F.op(args) there
```

A clause runs outside its handler: what it performs goes to the handlers
around the `handle`, even an operation of another effect the same
handler handles. Nesting a handler for `E` in one for `F` instead isn't
the same, since a clause of the inner one that performs `F` would be
caught by the outer.

`discontinue` continues `k` as if the `perform` it captured had been
`perform F.op(args)` instead, under the handlers that were around it, so
what `F.op` gives is what that `perform` gives. Its results must fit what
`k` takes. Like OCaml's `discontinue`, this is how a clause stops a
computation without dropping it: with an effect whose handlers clean up
and abort, a cancelled task unwinds through its own cleanup. The handlers
inside the task can also recover, by resuming, as from an error. Conduit
has no exceptions of its own, so the frontend says which effect it is.

## Why the body is a closure and not a block

A handler could be a 5th kind of tail with a nested body, but then a jump in the
body could target a label outside it. Once `perform` has captured the body
as a continuation, that jump would run code that's outside what was
captured. Making the body a function keeps rule 2 unchanged: labels never
cross a function boundary, so they never cross a handler boundary either.
Join points stay jumps, even inside handled code.

## Rules

7. **A continuation is used at most once,** by `resume` or `discontinue`.
   Using one that was already used traps at run time: Wasm stack switching does this itself, and
   the evidence-passing lowering keeps a "used" flag in each continuation.
   Continuations can be stored like any other value (captured by a
   closure, put in a field, an array, a ref or an `Any`), so a scheduler
   can keep paused computations in a queue.

   The verifier also rejects a function that visibly uses a continuation
   twice on one path (see "Continuations" in [verify.md](verify.md)). That
   catches most mistakes early, but it isn't what makes continuations
   safe: it misses uses through stored continuations, and it can reject
   code where two branches test the same condition and can never both
   run. A frontend can restructure such code, for example by moving the
   second branch into each arm of the first.
8. **`Handle`, `Perform`, `Resume` and `Discontinue` have effect
   `Unknown`,** like `Call`.
   Every pass already treats calls as able to do anything, so none of them
   needs a special case. This is about memory, and is separate from the
   effect types below.

## Example

A generator that a handler sums:

```
effect Yield { yield(i32) -> () }

handler Sum(acc: Ref(i32)) for Yield {
  yield(x: i32, k: Cont(() -> ())) =
    let old = ref_get(acc)
    let new = add(old, x)
    ref_set(acc, new)
    let = resume(k)
    jump ret()
  return() = jump ret()
}

fn produce() -> () {
  let = perform Yield.yield(1)
  let = perform Yield.yield(2)
  jump ret()
}

fn sum() -> i32 {
  let acc = ref_new(0)
  let body = closure produce()
  let = handle Sum(acc, body)
  let total = ref_get(acc)
  jump ret(total)
}
```

## Two kinds of clause a backend should notice

These are checked by looking at a clause's body:

- **Tail-resumptive:** the clause ends in `let r = resume(k, ..); jump ret(r)`
  and uses `k` nowhere else. This is the common case (state, readers,
  logging, the `yield` above). `perform` can then be a plain call to the
  clause, with no stack captured. Every way out of the clause must resume
  like this, and `conduit_ir::clause::plain_call` turns such a clause into
  that plain function: without `k`, giving what it resumed with.

  A way out may also be `let r = discontinue F.op(k, ..); jump ret(r)`.
  The plain function then performs `F.op(..)` itself, under the handlers
  of the `perform` it handles, and gives what that gives.
- **Aborting:** the clause never uses `k` (exceptions, early exit).
  `perform` unwinds to the handler and nothing is captured. What the clause
  gives is what the `handle` gives, without the return clause.
  `conduit_ir::clause::aborting` gives the clause without `k`.

## Effect types

Rows are on functions, closure types, continuation types and handlers,
and effect parameters on functions, data types and handlers; the
verifier checks them. A frontend that doesn't track effects writes
`does Any`. Passes and backends use rows to tell what can't perform:
`cse`, `handlers` and `direct`, evidence passing, and the cleanups of
aborts and exceptions (`conduit_ir::owned::may_unwind`).

Effect types say which effects a function can perform: the effects come
after the results, after `does`, joined with `+`.

```
fn fn1(%0: i32) -> (i32) does Effect20 + Effect21 { .. }
let %3: imm fn(i32) -> (i32) does Effect20 = closure fn2()
```

- **A row** is the effects after `does`: a set of effects and **effect
  parameters** `E1`, `E2`, .., or `Any`. The order doesn't matter, and
  each effect and parameter is in a row at most once. A function or type with no
  `does` has no effects. `Any` is for frontends without effect types: it
  can perform anything, which is what every function can do today.
- **Rows are on signatures:** a function's, a closure type's
  (`fn(..) -> (..) does ..`), a continuation type's and an extern's. An
  effect operation's has none. A continuation's row is that of its
  handler, since resuming it runs the rest of the body, which is still
  handled.
- **An extern's row** only limits who may call it: a `call_extern` may
  perform its row, so its caller's row must hold it, but the host
  performs nothing. Such an effect usually has no operations, like an
  `io` that only code allowed to do I/O has: `effect io {}`. An extern's
  row has no effect parameters, since externs aren't generic.
- **An extern's ABI** says how a backend passes its arguments and
  results. By default it's how the backend represents values: an object
  is its address, and a `host` value is the host's own reference (in
  Wasm, see "Host values" in [`../host/js.md`](../host/js.md)). With
  `#[abi(flat)]`, it's how C compilers for wasm32 pass
  them, for WASI and other C-like hosts. Only scalars, arrays of scalars
  and arrays of `[i8]` can be passed, and only scalars given back:
  - a scalar is itself, an `i8` or `i16` zero-extended;
  - an array is its elements' address, then its length, or the address
    alone for a `#[addr]` parameter, like `args_get`'s buffers;
  - an array of `[i8]` is the address and count of an iovec list, a list
    of (address, length) pairs the call makes;
  - the first result is the import's result, and each other result is an
    address to write it to, after the arguments.

  The host may write only to `mut` arrays, and only during the call. An
  `imm` parameter is the host's promise not to write it. Iovec lists and
  out-parameters go on the shadow stack, popped after the call (see the
  Wasm backend below). To the optimizer, every `call_extern` borrows its arguments
  and may touch any memory, whatever the ABI.
- **Exports** are the functions with an `#[export(..)]`, which a host
  calls (see the text format below). The Wasm backend exports those with
  an `#[export(wasm, ..)]`, under their names, and rejects any other kind,
  which is for another target. An export takes and gives host values as
  the host's own references, as an extern does.
- **Exports start with no handlers.** The host calling an exported
  function grants it the effects without operations in its row. An
  operation it doesn't handle itself traps, even when the host calls it
  back from inside an extern: it never reaches the handlers around that
  extern's call, which are behind the host's frame.
- **Effect parameters** work like mutability parameters. Functions, data
  declarations and handlers can have them, after their mutability
  parameters: `fn fn3['m1, E1]`, `data data10[E1]`. Each call, closure,
  data type and `handle` gives one row per parameter:
  `call fn3[mut, Effect20 + E2]`, `imm data10[Nothing]`, where `Nothing`
  is the empty row. The callee's rows are seen with the arguments in
  place, and rows that end up naming an effect twice merge, so
  `Effect20 + E1` with `E1` as `Effect20` is `Effect20`. A parameter
  given `Any` makes the whole row `Any`. A row can have several
  parameters: `compose[E1, E2]` of an `f` that does `E1` and a `g` that
  does `E2` does `E1 + E2`.
- **A handler declares a row,** the effects its clauses can perform, and
  `handle` takes its effects out of the body's row: the body can perform
  them and what the handler's row allows, and the `handle` performs only
  the handler's row.
- **`throw` is built in:** see "Exceptions from the host" below.

### Exceptions from the host

A host can throw an exception that isn't Conduit's from an extern, as any
JavaScript function can. Where one can come from is part of the types
(#59): an extern that may throw has the built-in effect `throw` in its
row,

```
#[link(wasm, module = "dom", name = "query")]
extern query(imm [i8]) -> (host) does throw
```

and so, like any effect in an extern's row, does every function that can
reach a call of it, and the type of every closure that can. `throw` is
built in: no `effect` declares it, and no effect can be named that. `Any`
has it. It's written first in a row: `does throw + Log`.

The rules, what catching and throwing from Conduit code are, and how
each implementation does them, are in [exceptions.md](exceptions.md). In
short: an extern without `throw` that throws is undefined behavior, and
with counts, what an exception unwinds is dropped.

Polymorphism over effects is what effect parameters are for. A handler
that runs a body which can also perform other effects passes those on.
Here the items have names, for reading; in the text format they're
numbered, like `Effect20`, `handler30` and `fn1`:

```
effect Yield {
  (i32) -> ()
}

handler adder[E1] for Yield does E1 env(ref i32) ops(add_yield) ret done

fn add_yield[E1](%0: ref i32, %1: i32, %2: cont() -> () does E1) -> () does E1 {
  let %3: i32 = ref_get(%0)
  let %4: i32 = add(%3, %1)
  let = ref_set(%0, %4)
  let = resume(%2)
  jump ret()
}

fn done[E1](%0: ref i32) -> () does E1 { jump ret() }

// Sums what `%0` yields, and performs what else it does.
fn sum[E1](%0: ro fn() -> () does Yield + E1) -> (i32) does E1 {
  let %1: ref i32 = ref_new(0i32)
  let = handle adder[E1](%1, %0)
  let %2: i32 = ref_get(%1)
  jump ret(%2)
}
```

A frontend that monomorphizes types can keep effect parameters, as it
keeps mutability parameters, so that one instance of a function like
`map` serves callbacks with any effects. An effect with type parameters
becomes one effect per instance, as a generic data type does, and so
does a handler, as the generic function it's in does.

An operation with type parameters of its own, like Bolt's
`fn fail[T](error: E) -> T` or `fn choose[T](a: T, b: T) -> T`, is erased
instead: what has one of its types is `ro any` in the operation's
signature, which holds both `imm` and `mut` boxes. A `perform` boxes what
it passes at the type it's used at and unboxes what it gets, and a
clause, with `k: cont(ro any) -> R`, serves every type. That's safe for
the same reason as the rest of erasure: the frontend's type checker
checked the clause for every `T`, so it can only resume with what it was
given at `T`, and the `unbox` gets the type that was boxed. It costs
little: a clause that never resumes, like `fail`'s, never gives a box,
and once `direct` and `inline` put a clause where it's performed,
`simplify` folds `unbox(box(x))` to `x`. One operation per instance
would avoid boxing, but needs the whole program to know the instances,
copies every clause per instance, and erased code would need an `any`
instance anyway. If an operation's type parameter has a constraint,
like a type class, the frontend passes its instance as an argument, a
data type of closures, as erased code does.
`tests/programs/handlers/erased_ops.conduit` has both kinds.

The verifier's rules are under "Effect types" in [verify.md](verify.md).
In short, every op's row must be in the function's, rows are compared by
inclusion with a parameter only in itself, and a closure or continuation
type with a smaller row is a subtype of one with a bigger row.

What effect types give Conduit:

- **Cheaper lowering.** The evidence-passing pass needs to know which
  functions can perform, and today it would have to find out over the
  whole program and assume the worst of every `apply`. With rows, it
  decides one function at a time: a function with no effects gets no
  handler vector, and an `apply` of a closure with no effects is a plain
  call. An effect-polymorphic function can be compiled once, passing
  evidence for its parameter, or once per row it's called with.
- **Knowing what suspends.** A function whose row has no suspending
  effect, like one for async code, never needs its stack captured.
- **No unhandled effects.** A function with no effects can't reach a
  `perform` without a handler, so the run-time error is left only for
  `Any` and for entry points that have effects.
- **Better analysis.** An `apply` of an `Fn(_, Imm)` with no effects writes
  nothing that existed before (see "`Imm` closures don't write" in
  [optimisations.md](../passes/optimisations.md)).

The verifier only checks rows. It never infers them, since the frontend's
type checker already has. Rows don't exist at run time: backends and the
evaluator ignore them, except to decide how to lower handlers.

The text format adds:

- `does row` after a signature's results, and before a function's `where`.
  Results are always in parentheses, so in
  `fn() -> (imm fn() -> () does Effect20)` the `does` is the inner
  closure's, and `fn() -> (imm fn() -> ()) does Effect20` gives it to the
  outer one.
- Rows written `Effect20 + E1`, `Nothing` and `Any`.
- Effect parameters, without a quote and starting with an uppercase
  letter, after the mutability parameters: any such name parses but
  `Nothing` and `Any`, and they print as `E1`, `E2`, .. by position.
  Mutability parameters keep the quote, so `['m1, E1]` has one of each.
  In a row, a name is a parameter if the item being read declares one of
  that name, and an effect otherwise. No effect is named `E` followed by
  digits, so `print` writes such an effect by its number. Effects are
  still numbered `effect20`, and named like any other item.
- Rows as arguments, after the mutabilities: `call fn3[mut, effect20 + E1]`,
  `closure fn3[Nothing]`, `handle handler30[E1](..)`, `imm data10[imm, Any]`.

Still to decide:

- Effects whose parameters are rows, like one whose operation starts a
  task that performs some row of effects.
- Externs that suspend, through JSPI.
- Whether handler clauses can also have mutability parameters, now that
  `handle` gives arguments.

### Why rows are sets

Row systems are usually built for inference: Koka's scoped labels unify
rows that can name an effect more than once, and Rémy-style rows, as in
OCaml's polymorphic variants, mark each label present or absent. Conduit
needs neither:

- **It only checks.** The frontend has inferred the rows, so the verifier
  only asks whether one row is in another. A parameter is an unknown row
  that's only in itself, so with any number of parameters, that's a test
  on two sets, with nothing to unify.
- **No effect is in a row twice.** Koka allows `<state, state | e>`
  because `mask` lets a `perform` skip the nearest handler of its effect
  and reach an outer one. Conduit has no `mask`: a `perform` always goes
  to the nearest handler, so when a parameter turns out to be an effect
  the body also names, both are caught by the same handler, and one
  `Effect20` in the row says just that. With `mask`, or handlers that are
  found lexically, rows would need scoped labels.

So a row is `Any`, or a set of effects and a set of parameters. An
implementation can keep them as sorted vectors with no duplicates: rows
are part of every closure and continuation type, which passes and query
systems compare and hash all the time, and sorted vectors make that
structural and cheap, and printing deterministic.

What would take more:

- **Effects with rows as parameters,** like one whose operation starts a
  task that performs some row: an effect in a row would then be an effect
  and its rows, compared structurally. Still sets.
- **Constraints,** like `where E1: Effect20`, if a frontend's type checker
  proves inclusions that the rows alone don't show. They'd be assumptions,
  like those of mutability parameters, added when a program needs them.

## Lowering

A backend either lowers handlers itself or runs evidence passing first,
which turns them into ordinary Conduit IR that any backend runs. How the
Wasm backend does it is in [wasm.md](../backends/wasm.md), and evidence passing in
[lowerings.md](../passes/lowerings.md).
