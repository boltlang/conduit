# Verification

`conduit_ir::verify` checks that IR follows the rules of [the IR](ir.md).
Every other part of Conduit assumes its input passed: the evaluator, the
optimizer and the Wasm backend only accept verified IR. What they do with
IR that doesn't pass is never a crash, but it can be an error or a wrong
result.

- **`verify_module(m)`** checks a whole module: its declarations, then
  every function.
- **`verify_func(p, id, f)`** checks one function, asking the `Program` `p`
  only for the signatures and declarations it uses (`func_sig`, `data`,
  `extern_fn`, `effect`, `handler`, `constant`, and `func_effect_params`). So a frontend can verify each
  function on its own, and a Salsa frontend only recompiles a function's
  check when what it uses changes. The declarations are assumed to be well
  formed.

Each `Error` names the item it's in (`fn1`, `data10`, `handler30`, ..), the
source of the nearest `let` or `jump` before it, if that has one, and a
message. The verifier reports every error it finds, not only the first.

The test programs in `tests/programs/invalid` each break one rule, with a
`//# verify-error:` check for the message.

## What it doesn't check

Some rules can't be checked from the IR alone. The frontend's type checker
has already proven them, and breaking one is **undefined behavior**:

- `Unbox(ty)` of a value that wasn't boxed at a subtype of `ty`.
- `ArrayFreeze` of an array that something else can still reach.
- `array_get` of an element that `array_alloc` made and nothing has set.
- Using what `undefined` gives: as the operand of an op that looks at it,
  or in a `case`. Passing it on, storing it or returning it is fine.
  Counting it is using it: `dup`, `drop`, dropping what holds it, or a
  `set` or `ref_set` that replaces it. What won't be used is `forget`ten
  (see "Reference counts" in [ir.md](ir.md)).
- Reaching `unreachable`. A program that means to stop there writes `trap`.
  The evaluator reports it as undefined behavior, and Wasm traps.

Others are checked at run time:

- Traps: a signed division that overflows, an index out of bounds, a float
  converted out of range, `trap`.
- A continuation resumed twice through a copy the verifier can't see, like
  one stored in a closure (see "Continuations").
- A `perform` with no handler. Effect types would check this (see "Effect
  types").

## Declarations

`verify_module` checks each declaration before the functions:

- **Data types:** every field type is well formed (see "Types"), and can
  use `inherit` and the declaration's mutability parameters.
- **Externs and effects:** every type in their signatures is well formed,
  with no `inherit` and no parameters. An extern with `#[abi(flat)]` only
  takes scalars, arrays of scalars and arrays of `[i8]`, and `#[nolen]`
  only on an array of scalars, and gives only scalars: no data, closures
  or host values. `#[nolen]` needs the flat ABI.
- **Handlers:**
  - The effect and the return clause exist.
  - The environment's types are well formed.
  - The return clause's parameters start with the environment.
  - There's one clause per operation of the effect. Clause `i` exists and
    has the type `fn(env.., op params.., cont(op results) -> (R)) -> (R)`,
    where `R` is what the return clause gives.
  - No clause has mutability parameters, since nothing would give them
    arguments.
- **Constants:**
  - The type is well formed, with no `inherit` and no parameters, and
    `imm` all the way down, since every use shares the one object.
  - The value is of that type: a literal of a number type, an array of
    values of its element type, `C(..)` with a value per field of `C`, as
    the type's mutability arguments make them, or another constant whose
    type is a subtype.
  - No constant refers to itself, through the constants it's made of.

## Types

A type is well formed when:

- every data type it names exists, and gets one mutability argument per
  parameter of its declaration;
- `inherit` is only in a field type or in an array's element type;
- a mutability parameter `'mi` is one of the parameters in scope: those of
  the data declaration for a field type, or of the function for a type in
  a function. There's no `'m0`.

Every variable's type and every result type of a function must be well
formed, as must the type in `box[ty]` and `unbox[ty]`.

## Scope

These are rules 1 to 4 of the IR.

- **Variables** are bound once each, by a function parameter, a join
  parameter or a `let`, and must have a type in the function. A variable is
  only used where it's in scope: in the rest of its block and in what's
  nested in it.
- **Labels** are `j1` to `j(n-1)`, for a function that declares `n` labels.
  `ret` is label 0, is always in scope, and can't be bound by a `join`. Each
  label is bound once. A non-recursive `join` puts its labels in scope in
  its body, and a recursive one in its definitions too.
- **Jumps** target a label in scope, with arguments that fit the label's
  parameter types. `ret`'s parameter types are the function's results.
- A function has at least the label `ret`.

## Cases

- **On an integer:** each tag is an integer that fits the scrutinee's type
  read as signed, so an `i8`'s tags are `-128` to `127`. No tag is there
  twice, and there's a default.
- **On a data value:** each tag is a constructor of the scrutinee's type,
  none is there twice, and either every constructor has an arm or there's a
  default.
- Nothing else can be branched on.

## Operations

Each op checks its arguments and gives result types. A `let` binds as many
variables as the op gives values, and each result must fit its variable's
type (see "Subtyping"). Some ops take a type from the variable they bind,
where the op doesn't spell it out: `new`, `closure`, the array ops that make
an array, and `ref_new`.

| Op | Arguments | Gives |
|---|---|---|
| `add`, `sub`, `mul`, `div` | two numbers of the same type | that type |
| `rem`, `and`, `or`, `xor`, `shl`, `shr` | two integers of the same type | that type |
| `min`, `max` | two floats of the same type | that type |
| `eq`, `ne`, `lt`, `le`, `gt`, `ge` | two numbers of the same type | `i32` |
| `neg` | a number | its type |
| `abs`, `sqrt` | a float | its type |
| `convert[to]` | a number; `to` a number type | `to` |
| `reinterpret[to]` | a number; `to` of the same size, an integer for a float or a float for an integer | `to` |
| `call f[margs]` | what `f` takes, with `margs` | what `f` gives |
| `call_extern e` | what `e` takes | what `e` gives |
| `closure f[margs]` | the first values `f` takes, as captures | the `fn` it binds, which takes and gives the rest of `f`'s signature |
| `apply` | a closure, then what it takes | what it gives |
| `new C` | a value per field of `C` | the data type it binds, of `C`'s type |
| `const c` | nothing | `c`'s type |
| `undefined` | nothing | the type it binds |
| `forget` | an object | nothing |
| `get C.i` | an object of `C`'s type | field `i` |
| `set C.i` | a `mut` object of `C`'s type, then a value for field `i` | nothing |
| `array_new` | elements | the array it binds |
| `array_fill` | an `i32` length, then an element | the array it binds |
| `array_alloc` | an `i32` length | the array it binds, which is `mut` |
| `array_get` | an array and an `i32` index | an element |
| `array_set` | a `mut` array, an `i32` index, then an element | nothing |
| `array_len` | an array | `i32` |
| `array_copy` | a `mut` array, an `i32` start, an array whose elements fit the first's, an `i32` start and an `i32` count | nothing |
| `array_clone` | an array whose elements fit the array it binds | the array it binds |
| `array_freeze` | the same, binding an `imm` array | the array it binds |
| `ref_new` | a value that fits the ref it binds | the ref it binds |
| `ref_get` | a ref | what it holds |
| `ref_set` | a ref, then a value that fits it | nothing |
| `box[ty]` | a value that fits `ty` | `imm any` if `ty` is `imm` all the way down, else `mut any` |
| `unbox[ty]` | an `any` | `ty` |
| `handle H` | `H`'s environment, then a `fn() -> (body results)`, the results being what `H`'s return clause takes after the environment | what the return clause gives |
| `perform E.i` | what operation `i` of `E` takes | what it gives |
| `resume` | a continuation, then what it takes | what it gives |
| `discontinue F.i` | a continuation, then what operation `i` of `F` takes; what the operation gives must fit what the continuation takes | what the continuation gives |
| `dup`, `drop`, `free`, `decref` | an object: a value of any type but a scalar | nothing |
| `is_unique` | an object | `i32` |
| `drop_reuse`, `reuse_token` | a data object known to be built by `C` (as for `get C.i`) | the `reuse C` it binds |
| `reuse C'` | a `reuse C`, where `C` has the same shape as `C'`, then what `new C'` takes | what `new C'` gives |
| `host_null` | nothing | `nullable host` |
| `host_is_null` | a `host` or `nullable host` | `i32` |
| `host_unwrap` | a `host` or `nullable host` | `host` |
| `host_function` | a closure whose parameters and results are all scalars, `host` or `nullable host` | `host` |

Field accesses also follow rule 5: `get C.i` and `set C.i` on `x` need `x`
to be known to be built by `C`: because `C`'s type has only one
constructor, because `x` is bound by `new C` or by `const c` of a
constant built by `C`, or because the access is in an arm `C` of a `case`
on `x`. A constructor never changes, so this holds
for `mut` objects too.

## Subtyping

A value fits where a type is expected when its type is a subtype of it.
That's about an object's own mutability, `imm` and `mut` both fit `ro`,
about rows (see "Effect types"), and about host values: a `host` fits
where a `nullable host` is expected. Nothing else differs. A data type's arguments must be equal, as
must an array's element type and a closure's signature. A `mut` object's
fields can be written, so a covariant argument would let code store a `mut`
value where another alias expects `imm`.

Subtyping applies to the arguments of `call`, `call_extern`, `apply`,
`perform`, `resume`, `jump` and `handle`, to fields and elements stored by
`new`, `set`, the array ops and `ref_new`/`ref_set`, and to what a `let`
binds. Nothing else converts between mutabilities.

## Mutability

These rules are what make `imm` mean that nothing ever changes.

1. **An object holds only what is at most its mutability,** all the way
   down, in the order `imm ⊑ ro ⊑ mut`. So an `imm` object or closure holds
   or captures only scalars and `imm` values: never a ref, a continuation,
   or anything `mut` or `ro`. A `mut` one can hold anything. This is checked
   at `new`, `array_new`, `array_fill`, `array_alloc`, `array_clone`,
   `array_freeze` and `closure`, and decides what `box` gives. A data type that's being
   checked already counts as fine where it recurses, so recursive types
   terminate.
2. **Only `mut` can be written.** `set` and `array_set` need a `mut` object.
   `ref_set` always works, since refs are always `mut`.
3. **New objects are `imm`, `mut` or a mutability parameter,** never `ro`
   or `inherit`, which are views and not mutabilities an object has. That
   applies to what `new`, `closure` and the array ops bind.

## Mutability parameters

- A function's assumptions only name `imm`, `mut`, `ro` and its own
  parameters.
- Each `call` and `closure` gives one argument per parameter of its
  callee: `imm`, `mut` or a parameter of the caller, never `ro` or
  `inherit`. The callee's signature is then checked with the arguments in
  place of its parameters.
- Each of the callee's assumptions `a: b` must hold with the arguments in
  place, given the caller's own assumptions.
- Inside the function, a parameter may be `imm`, so it can't be written,
  and holding follows rule 1 with what the function assumes: a `'m1` object
  can hold a `'m2` value only if `'m2: 'm1` is assumed.

## Continuations

Rule 7 of the IR says a continuation is used at most once, which the run
time checks. The verifier also rejects a function that can use a
continuation twice in a way it can see:

- Each time it's an argument counts as a use: of `resume` and
  `discontinue`, but also of
  any other op, like a call, `closure` or `new`, and of a jump. `dup`,
  `drop`, `is_unique`, `free` and `decref` don't count: they only count its
  references.
- Uses add up along a path. A `case` takes the most of its arms, and a jump
  adds what the join point's body uses of variables bound outside it.
  Recursive join points are counted until the counts stop changing, so a
  loop that resumes the same continuation on each round counts twice.

This misses uses through stored continuations, which the run-time check
catches instead. And it doesn't know which paths can run, so it can reject
code whose two uses are on branches of the same condition that never both
run.

## Effect types

See "Effect types" in [the IR](ir.md#effect-types).

Every function, closure type and continuation type has a row of effects
after `does`. The verifier checks that each `perform` is in a function
whose row allows it. So a function with no effects can't reach a `perform`
without a handler: only `Any` and entry points with effects still can.

**Inclusion.** A row `R1` is in a row `R2` when every effect of `R1` is in
`R2`, and every parameter of `R1` is a parameter of `R2`. A parameter is an
unknown row: it's only in itself, and in rows that name it. Every row is in
`Any`, and `Any` is only in `Any`. This is a test on two sets: the
verifier never solves for a parameter.

**Rows.** A row has each effect and each effect parameter at most once.
Its effects must exist, and its parameters must be in scope: `E1` to `En`
for a function, data declaration or handler with `n` effect parameters.

**Substitution.** A call, closure, data type or `handle` gives a row for
each parameter. Putting them in a row replaces each parameter with its
row's effects and parameters, merging what's named twice, or makes the
whole row `Any` if one of them is `Any`.

**What an op performs.** Each op has a row:

| Op | Row |
|---|---|
| `perform E.i` | `E` |
| `call f[args]` | `f`'s row, with the arguments in place of its parameters |
| `apply(c, ..)` | the row of `c`'s type |
| `resume(k, ..)` | the row of `k`'s type |
| `discontinue F.i(k, ..)` | the row of `k`'s type, and `F` |
| `handle H[args](..)` | the row of `H`, with the arguments in place |
| `call_extern` and every other op | `Nothing` |

**Functions.** The row of each op in a function's body is in the
function's row. Join points are part of the function, so they need no rule
of their own.

**Calls and closures.** Effect arguments are rows, checked like those
written in types. `closure f[args]` binds a `fn` whose row has `f`'s row in
it, with the arguments in place.

**Subtyping.** A closure or continuation type fits where one with a bigger
row is expected: `fn() -> () does Effect20` fits where
`fn() -> () does Effect20 + Effect21` is expected, and everything fits a
row of `Any`. Mutability is unchanged. Only the type's own row can be
bigger: rows inside it, in its parameters or results or in a data type's
arguments, must be equal, like mutability arguments.

**Handlers.** A handler `H` for effects `E` (one or more, each once, with
a clause for each of their operations) declares a row `R`, the effects
its clauses can perform.

- Every clause and the return clause has the handler's effect parameters,
  in the same order, which `handle` gives them, and a row that's in `R`.
- The continuation a clause gets has the row `R`, since resuming it runs
  the rest of the body, which is still handled by `H`.
- `handle H[args](env.., body)` needs the body's row to be in `E + R`.

So `handle` takes the effects in `E` out of what the body performs, and puts back what
the clauses perform.

**Externs** have no effects: a host function can't perform a Conduit
effect.
