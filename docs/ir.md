# Conduit IR (draft)

Conduit's IR is a typed, monomorphic, A-normal-form tree with join points,
with a boxed `Any` type for code that can't be monomorphized.
It is meant to be small: a pass handles **5 kinds of term**. Everything that
isn't control flow is a single `Let` node holding an operation, so a pass that
doesn't care about a particular operation never has to match on it.

## Goals

- **Few cases.** Control flow has 5 term kinds. Operands have 2 kinds. Every
  operation has the same shape, `let xs = op(args)`.
- **Correct by construction where possible.** A jump can only appear in tail
  position, because the grammar has no other place to put one. The only
  checks left for join points are "is the label in scope?" and "do the
  arguments match?"
- **Easy to lower to Wasm.** Join point scoping matches Wasm's
  `block`/`loop`/`br` nesting, so no relooper is needed. Lowering to a flat
  CFG with block parameters (Cranelift, LLVM) is mechanical.
- **Room for real languages.** ADTs, closures, mutable fields, arrays,
  GC'd values and effect handlers. See "What frontends need" for what a
  frontend's own mid-level IR usually carries that Conduit must carry too.
- **Handlers add no terms.** Effect handlers are three more `Op`s and one
  more type. Passes that ignore them keep working unchanged.

## The whole IR

```rust
/// An operand: never a nested expression.
pub enum Atom {
    Var(Var),
    Lit(Lit),          // integer or float constant
}

pub enum Term {
    /// `let x1, .., xn = op(a1, .., am); body`
    Let { binds: Vec<Var>, op: Op, args: Vec<Atom>, body: Box<Term>, source: Option<SourceId> },

    /// `join [rec] j1(params) = t1, .., jn(params) = tn in body`
    Join { rec: bool, defs: Vec<JoinDef>, body: Box<Term> },

    /// `jump j(a1, .., an)`, which also covers return: see "Returning" below.
    Jump { target: Label, args: Vec<Atom>, source: Option<SourceId> },

    /// Branch on an integer or on the constructor of an ADT value.
    /// `default` runs when no arm's tag matches.
    Case { scrut: Atom, arms: Vec<Arm>, default: Option<Box<Term>> },

    /// This point is never reached (a failed match, a call to a `!` function).
    Unreachable,
}

pub struct JoinDef { label: Label, params: Vec<Var>, body: Term }

pub struct Arm { tag: Tag, body: Term }

pub enum Tag { Int(i64), Ctor(CtorId) }

pub struct CtorId { data: DataId, index: u32 }
```

`Var` and `Label` are indices into the function: a function stores the
type of each variable, and how many labels it uses. A label's parameter
types are those of its join definition's parameters. `source` is an
opaque ID the frontend uses to map code back to its source.

### Operations

All non-control work goes through `Op`. Passes that move code around
(inlining, renaming, free variables, liveness, dead code elimination) don't
look inside it. They only ask it a few questions:

```rust
pub enum Op {
    Prim(Prim),            // add, sub, mul, div, eq, lt, conversions, ...
    Call(FuncId),          // direct call
    CallExtern(ExternId),  // host function (FFI, runtime builtins)
    Closure(FuncId),       // args = captured values; result has type Fn(sig)
    Apply,                 // args[0] = closure, rest = arguments
    New(CtorId),           // allocate an ADT value; args = its fields
    Get(CtorId, u32),      // read field i
    Set(CtorId, u32),      // write field i of a Mut object; binds nothing
    ArrayNew,              // args = the elements
    ArrayFill,             // args = a length, then the value to repeat
    ArrayGet, ArraySet, ArrayLen,   // ArraySet: Mut arrays only
    RefNew,                // args = initial value; result has type Ref(ty)
    RefGet,                // args = the ref
    RefSet,                // args = the ref, then the new value; binds nothing
    Box(Ty),               // args = a value of that type; result has type Any(_)
    Unbox(Ty),             // args = an Any(_); result has that type
    Copy(Mutability),      // args = a value; a copy at that mutability: see "Mutability"
    Handle(HandlerId),     // args = handler env, then a closure: the body
    Perform(EffectId, u32),// run operation i of the nearest handler of the effect
    Resume,                // args[0] = continuation, rest = values to resume with
}

impl Op {
    fn effects(&self) -> Effects;          // Pure | Reads | Writes | Unknown
    fn result_types(&self, ..) -> Vec<Ty>;
}
```

Adding an operation means adding an `Op` variant and its `effects()`. No
control-flow pass needs to change.

An op that can trap has effect `Unknown`, so that dead-code elimination
never removes a trap: integer division and remainder (on zero), float to
integer conversions (out of range), array accesses (out of bounds), and
`Unbox` (a failed cast).

### Types

```rust
pub enum Ty {
    I32, I64, F32, F64,
    Data(DataId, Mutability),     // GC'd ADT value
    Array(Box<Ty>, Mutability),   // GC'd array
    Fn(Box<Sig>, Mutability),     // closure
    Cont(Box<Sig>),               // one-shot continuation captured by a handler
    Ref(Box<Ty>),                 // mutable cell holding one value: always Mut
    Any(Mutability),              // a boxed value of any type: see "Polymorphism"
}

pub enum Mutability { Imm, Mut, ReadOnly, Inherit }

impl Ty {
    fn mutability(&self) -> Option<Mutability>;   // None for scalars and Cont
    fn with_mutability(&self, m: Mutability) -> Ty;
}

pub struct DataDecl { ctors: Vec<CtorDecl> }
pub struct CtorDecl { fields: Vec<Ty> }
```

Mutability is part of the types that are objects: see "Mutability".
`Inherit` is only valid in the field types of a `CtorDecl`.

A tuple or struct is a `DataDecl` with one constructor. `Bool` and
field-less enums are `I32`. Unit is zero values and `!` is zero results, so
neither needs a type of its own: every binder list, parameter list and
result list can be empty.

### Modules

```rust
pub struct Module {
    data: Vec<DataDecl>,
    funcs: Vec<Func>,
    externs: Vec<Extern>,  // signature + host name
    effects: Vec<EffectDecl>,
    handlers: Vec<HandlerDecl>,
}

pub struct Func {
    params: Vec<Var>,
    results: Vec<Ty>,
    body: Term,            // `ret` is in scope: see below
}
```

Functions exist only at the top level.

### Closures

Closure conversion is split between the frontend and Conduit:

| Job | Who |
|---|---|
| Move nested functions to the top level, and list what each one captures | Frontend |
| Lay out closures in memory, generate wrappers, decide how calls work | Conduit |

The frontend lambda-lifts: each nested function becomes a top-level `Func`
whose **captured values come first, followed by its own parameters**. Where
the source creates the function value, the frontend emits
`let c = closure f(captures..)`, and where it calls a function value it
emits `let r = apply(c, args..)`. The frontend lists every variable the
function uses from its surroundings, without checking which are assigned:
mutable locals are already refs (rule 6), so the closure captures the ref.

```
// fn make_adder(n: i32) -> (i32 -> i32) { |x| x + n }

fn adder(n: i32, x: i32) -> i32 {      // capture first, then parameter
  let r = add(x, n)
  jump ret(r)
}

fn make_adder(n: i32) -> Fn(i32 -> i32) {
  let c = closure adder(n)
  jump ret(c)
}
```

Nested functions aren't allowed in the IR. A function body would have to sit
inside an `Op`, so ops would stop being opaque, passes would have to handle a
scope inside a scope, and rule 2 (labels never cross a function boundary)
would need checking instead of holding by construction. Conduit doesn't
lose optimizations by this, since `Closure(f)` names the function: see
"Known closures" under "Optimizations".

### Polymorphism

Conduit is monomorphic: functions have no type parameters, and every type
is known. Frontends monomorphize, which gives fast, unboxed code. That
fails for **polymorphic recursion**, where a function calls itself at a
different type:

```
data Nested a = Flat a | Nest (Nested (a, a))

fn depth<a>(n: Nested a) -> i32 {
  match n {
    Flat _ => 1
    Nest m => 1 + depth::<(a, a)>(m)   // calls itself at (a, a)
  }
}
```

`depth<i32>` needs `depth<(i32, i32)>`, which needs
`depth<((i32, i32), (i32, i32))>`, and so on without end. For such code,
Conduit offers a uniform representation instead of generics, as OCaml and
GHC use for polymorphic code:

- `Any` is a boxed value of any type.
- `Box(ty)` turns a value of type `ty` into an `Any`, and `Unbox(ty)` turns
  it back. The `Any` is `Any(Imm)` when `ty` is `Imm` all the way down
  (scalars included), and `Any(Mut)` otherwise. For `Data`, `Array`, `Fn`, `Cont` and `Ref` values, which are
  already references, both are free or a cast. Scalars are allocated.

The frontend uses them like this:

1. **Monomorphize when the instances are finite.** This covers nearly all
   code.
2. **Erase the rest.** A function that recurses polymorphically, together
   with the functions it's mutually recursive with, is compiled once, with
   every type variable replaced by `Any`: `Any(Imm)` if the variable is
   only ever instantiated with types that are `Imm` all the way down, and
   `Any(Mut)` otherwise. The type checker already knows
   these functions: in Hindley-Milner, polymorphic recursion only
   type-checks with an explicit signature, and the recursive call
   instantiates that signature at a different type.
3. **Box at the boundary.** A monomorphic caller boxes the arguments it
   passes to an erased function where the parameter type is a type
   variable, and unboxes the results.
4. **Pass type classes as dictionaries.** In erased code, the frontend
   can't pick an instance by type, so it passes instances as values: a
   `DataDecl` whose fields are closures, one per method.

`Unbox(ty)` on a value that wasn't boxed at type `ty` is undefined: it
traps with Wasm GC and goes unchecked with linear memory. Conduit can't
verify it. The frontend's type checker has already proven it correct.
`Any` also covers existential types and dynamically typed values, which
a monomorphic IR can't express otherwise.

Generic code (type parameters in the IR) is left out on purpose: every
pass would have to handle types it doesn't know, inlining would substitute
types, layouts wouldn't be known at compile time, and the Wasm backend
would erase the types anyway.

### Mutability

Every object (an ADT value, an array, a closure) is either `Imm` or `Mut`.
`Imm` is **deep**: nothing reachable from an `Imm` value ever changes.
"Reachable" means everything found by starting at the value and following
references: fields, array elements, what a closure captured, what a ref
holds, and so on. Scalars and `Cont` have no mutability.

| Type | `Imm` | `Mut` |
|---|---|---|
| `Data`, `Array` | can't be written, and holds only `Imm` values | `Set`/`ArraySet` are allowed, and it can hold anything |
| `Fn` | captured only values that are `Imm` all the way down | captured anything, including `Mut` objects and refs |
| `Any` | holds a value that is `Imm` all the way down | holds anything |

A closure never changes after it's created, so on `Fn` the flag only
limits what the closure can reach. A closure that captures a ref is
always `Mut`. A closure can be `Mut` even if it never writes:

```
let p = new Point(0, 0)                // p: Data(Point, Mut)
let pair = new Pair(p, 5)              // holds p
let r = closure read_x(pair)           // read_x only reads pair.0.x
```

`r` reaches `p` through `pair`, and `p` can change, so calling `r` twice
with the same arguments can give different answers. `r` must be
`Fn(_, Mut)`.

The other two mutabilities:

- **`ReadOnly`** is a view that accepts both: an `Imm` or a `Mut` value can
  be passed where a `ReadOnly` one is expected. It can't be written, but
  unlike `Imm` it doesn't promise that nothing else changes it.
- **`Inherit`**, only in a `CtorDecl`'s field types, means "the mutability
  of the object holding this field". A `Get` on a `Data(D, m)` gives a field
  of type `Data(E, Inherit)` the type `Data(E, m)`. Fields with an explicit
  mutability keep it.

Rules the verifier checks:

1. **Deep `Imm`.** An `Imm` object or closure holds or captures only values
   that are `Imm` all the way down: scalars, and `Data`, `Array`, `Fn` and
   `Any` with `Imm`. It never holds a `Ref`, a `Cont`, or anything `Mut` or
   `ReadOnly`. This is checked at `New`, `ArrayNew`, `Closure` and `Box`.
2. **Only `Mut` can be written.** `Set` and `ArraySet` need a `Mut` object.
   `RefSet` always works, since refs are always `Mut`.
3. **Subtyping.** `Imm ≤ ReadOnly` and `Mut ≤ ReadOnly`. An argument to
   `Call`, `Apply`, `Jump` or `New` may have a subtype of the expected
   type. Nothing else converts between mutabilities: that's `Copy`.

### Copying between mutabilities

A `Mut` value can't become `Imm` in place, since a `Mut` reference to it
may still exist. `Copy` makes a new value instead:

- **`Copy(Imm)`** copies every `Mut` `Data` and `Array` object the value
  reaches into a new `Imm` one, and shares what's already `Imm`. Each
  object is copied once, even if it's reachable along several paths, so
  sharing and cycles stay intact. It doesn't follow closures: the verifier
  rejects a `Copy(Imm)` whose value can reach a `Fn(_, Mut)`, a `Ref`, a
  `Cont` or an `Any(Mut)`.
- **`Copy(Mut)`** goes the other way: it copies the `Imm` `Data` and `Array`
  objects it reaches into new `Mut` ones and keeps what's already `Mut`.
  Closures are kept as they are.
- The result type is the argument's type with the mutability of each
  `Data` and `Array` it reaches changed.
- On a `ReadOnly` value, Conduit can't tell which objects are already
  `Imm`, so `Copy` copies all of them.

`Copy` is a single op rather than a call to a generated function because
removing copies is the main optimization (see "Optimizations"), and an
analysis can only remove what it can recognize. Its effect is `Reads`: it
allocates, but it writes nothing that existed before.

## Rules

1. **Variables are SSA.** Each one is bound once, by a function parameter, a
   join parameter or a `Let`. It is in scope in the term below its binder.
2. **Labels are scoped.** A non-recursive `Join` puts its labels in scope
   in `body`. A recursive one also puts them in scope in every `defs[i].body`.
3. **Jumps are tail calls by construction.** `Jump` is a `Term`, and terms
   only appear in tail positions: a `Let` body, a `Case` arm, a join
   definition or a join body. There is no `let x = case ...`. Nothing needs to
   check for escaping join points, and no pass can accidentally make one.
4. **Returning is jumping.** Every function body has the label `ret` in
   scope, whose parameters are the function's results. So return needs no
   term of its own. A tail call is just `let r = call f(..); jump ret(r)`,
   which a backend can recognize and emit as `return_call`.
5. **Field reads happen where they're written.** A `Case` on an ADT binds
   no fields. `Get(C, i)` on `x` is valid in an arm `C` of a case on `x`,
   or when `x`'s type has a single constructor. Fields aren't bound in the
   arm because fields can be mutable: a binding made at the `case` would
   be a snapshot that goes stale after a later `Set`.
6. **Mutable locals are refs.** Variables are never assigned, so a frontend
   turns each mutable local of the source language into a `Ref`: `RefNew`
   where it's declared, `RefGet` where it's read and `RefSet` where it's
   assigned. A closure that uses the local captures the ref like any other
   value. The frontend doesn't need to convert to SSA, analyze captures or
   declare cell types: the "promote refs" pass (see "Optimizations") turns
   every ref that doesn't escape into join point parameters.

   For example, `var i = 0; while i < n { i = i + 1 }; return i` can be
   given to Conduit as:

   ```
   let i = ref_new(0)
   join rec loop() =
     let v = ref_get(i)
     let c = lt(v, n)
     case c {
       1 => let v2 = add(v, 1)
            ref_set(i, v2)
            jump loop()
       _ => let r = ref_get(i)
            jump ret(r)
     }
   in jump loop()
   ```

   After promotion, `i` is a parameter of `loop`, as in `sum` below.

## Examples

`let x = if c { a } else { b }; x + 1`:

```
join k(x: i32) =
  let y = add(x, 1)
  jump ret(y)
in case c {
  1 => jump k(a)
  _ => jump k(b)
}
```

Factorial, matching on `0 | 1`:

```
fn fac(n: i32) -> i32 {
  join one() = jump ret(1)
  in case n {
    0 => jump one()
    1 => jump one()
    _ => let k = sub(n, 1)
         let r = call fac(k)
         let m = mul(n, r)
         jump ret(m)
  }
}
```

A loop with two mutable locals:

```
fn sum(n: i32) -> i32 {
  join rec loop(i: i32, acc: i32) =
    let done = ge(i, n)
    case done {
      1 => jump ret(acc)
      _ => let i2 = add(i, 1)
           let acc2 = add(acc, i)
           jump loop(i2, acc2)
    }
  in jump loop(0, 0)
}
```

A match on an ADT:

```
fn unwrap_or(o: Option_i32, d: i32) -> i32 {
  case o {
    Some => let v = get Some.0(o)
            jump ret(v)
    _    => jump ret(d)
  }
}
```

## Effect handlers

Handlers are **deep** (a resumed computation is still handled by the same
handler) and continuations are **one-shot** (each can be
resumed at most once). The IR has mutation, and resuming a continuation
twice would run its writes twice. One-shot continuations are also what
Wasm stack switching provides.

### Declarations

```rust
pub struct EffectDecl {
    ops: Vec<Sig>,         // operation i: its parameters and results
}

pub struct HandlerDecl {
    effect: EffectId,
    env: Vec<Ty>,          // values the clauses share, like a closure's
    ops: Vec<FuncId>,      // clause i: fn(env.., op params.., k: Cont) -> R
    ret: FuncId,           // return clause: fn(env.., body results..) -> R
}
```

`R` is the result of the whole `handle`. The continuation `k` has type
`Cont(op results -> R)`: resuming it with the operation's results runs the
rest of the body, through the return clause, and gives back an `R`.

### The three operations

```
let r = handle H(env.., body)   -- run closure `body` under handler H
let x = perform E.op(args)      -- nearest enclosing handler of E (dynamic)
let r = resume(k, vals)         -- continue a captured computation
```

### Why the body is a closure and not a term

A handler could be a 6th term with a nested body, but then a jump in the
body could target a label outside it. Once `perform` has captured the body
as a continuation, that jump would run code that's outside what was
captured. Making the body a function keeps rule 2 unchanged: labels never
cross a function boundary, so they never cross a handler boundary either.
Join points stay jumps, even inside handled code.

### Rules

7. **`Cont` values are linear.** Each continuation is used at most once on
   every path: one `resume`, or dropped. A verifier checks this the same way
   it checks scoping.
8. **`Handle`, `Perform` and `Resume` have effect `Unknown`,** like `Call`.
   Every pass already treats calls as able to do anything, so none of them
   needs a special case.

### Example

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

### Two kinds of clause a backend should notice

These are checked by looking at a clause's body:

- **Tail-resumptive:** the clause ends in `let r = resume(k, ..); jump ret(r)`
  and uses `k` nowhere else. This is the common case (state, readers,
  logging, the `yield` above). `perform` can then be a plain call to the
  clause, with no stack captured.
- **Aborting:** the clause never uses `k` (exceptions, early exit).
  `perform` unwinds to the handler and nothing is captured.

### Lowering

There are two ways to lower handlers. Both are possible because handlers
are confined to three ops.

- **Wasm stack switching** (the typed-continuations proposal): `handle` is
  `cont.new` + `resume` with an `(on $op ..)` clause per operation,
  `perform` is `suspend`, and `Resume` is `resume`. One-shot continuations
  match the proposal exactly. Browser support is still uncertain, so this
  backend should be optional.
- **Evidence passing** (Xie & Leijen, "Generalized evidence passing for
  effect handlers", and Xie et al., "Effect handlers, evidently"): a
  Conduit-to-Conduit pass that removes the three ops. It
  passes a handler vector to functions that may perform, calls
  tail-resumptive clauses directly, and turns code after a non-tail
  `perform` into a closure (selective CPS). The output is ordinary Conduit
  IR, so every backend works with it, including native ones without stack
  switching. Known-handler optimizations come from inlining after this
  pass.

## Optimizations come down to a few rewrites

Because the IR is so small, the classic simplifications are local rewrites:

- **Case of known constructor or literal.** If `case x` has a scrutinee
  bound by `New(C)` or a literal, replace the case with the matching arm.
  `Get(C, i)` of a known `New` of an `Imm` object becomes its argument.
- **Case-of-case.** In A-normal form with join points, this becomes "a jump
  to a known join point with a known argument". Inline (copy) the join
  body at that jump, substituting the arguments, and the case inside it
  becomes a case of a known value. Case-of-case falls out of inlining join
  points plus the previous rule.
- **Inlining functions.** Substitute the body and rename `ret` to a fresh
  join point that holds the rest of the caller. This is the same shape as
  any other join point.
- **Known closures.** When `apply(c, args..)` has a `c` bound by
  `closure f(captures..)`, replace it with `call f(captures.., args..)`.
  Because of the parameter order (captures first), this is just putting two
  argument lists together. The result is a plain `Call`, so inlining
  applies to it, and if `c` has no uses left, dead-code elimination removes
  the allocation.
- **Dead code.** Drop a `Let` whose results are unused and whose
  `op.effects()` is `Pure`, and drop a `Join` whose labels are unused.
- **Promote refs** (like LLVM's `mem2reg`). A ref that doesn't escape is
  one whose only uses are as the first argument of `RefGet` and `RefSet`
  in the function that creates it. It isn't captured by `Closure`, passed
  to a call or a jump, stored with `New`/`Set`/`ArraySet`/`RefSet`, or
  returned. Promote it like this: every join point it's live into gets an
  extra parameter for its current value, every `Jump` passes the current
  value, `RefGet` becomes that value, and `RefSet` makes a new value
  current. A ref that does escape stays a heap cell.
- **Remove copies.** In order of how easy they are:
  - If the type shows the value reaches nothing `Copy` would change (for
    `Copy(Imm)`: nothing `Mut`), the copy is the value itself.
  - `Copy(Imm)` of a value just built by `New` or `ArrayNew`, with no other
    uses, becomes building it as `Imm` directly.
  - **Freeze in place.** If nothing else can reach the original objects
    and the original isn't used afterwards, the copy can reuse them. This
    needs a region or uniqueness analysis that shows no other reference,
    closure, ref or `Any` can still reach them. The analysis follows what
    each closure captures through `Closure`'s arguments, not through the
    closure's type.
- **`Imm` closures don't write.** An `Apply` on an `Fn(_, Imm)` whose
  arguments are all `Imm` all the way down allocates and may perform
  effects, but writes nothing that existed before. So an analysis can treat
  it as `Reads`, unless it performs, instead of `Unknown`.

A pass is usually a single recursive function with 5 arms, and most passes
only do real work in one or two of them.

## Lowering

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

**To a flat CFG (Cranelift, LLVM).** Each join definition becomes a block
whose parameters are the join's parameters, `jump` becomes a jump with
block arguments, and `case` becomes `br_table` or `brif`. Function
bodies become the entry block.

### Closures

The code calling an unknown closure only has the closure and the
arguments: it doesn't know what was captured. So for each `Func` used in
a `Closure`, the backend generates a wrapper `f_entry(env, args..)`. It
reads the captures out of the closure object `env` and calls
`f(captures.., args..)`. A closure object holds a reference to its
wrapper (a table index with linear memory, a `ref.func` with Wasm GC),
followed by the captured values ("flat" closures). `apply(c, args..)`
loads the wrapper from `c` and calls it with `c` and `args`. Calls
rewritten by "Known closures" skip the wrapper.

## Operations per target

Only Wasm is a target for now. Since open question 1 (GC) isn't decided, the
table gives both ways to compile to it:

- **Linear memory:** Wasm 2.0, with values in memory from a Conduit
  allocator.
- **Wasm GC:** Wasm 3.0, with values as GC'd `struct`s and `array`s.

Status:

- ✅ supported directly
- 🔧 supported through a Conduit pass
- ⚠️ needs a Wasm proposal that isn't standard yet

| Operation | Linear memory | Wasm GC | Status |
|---|---|---|---|
| `Prim`: integer arithmetic, bitwise ops | `i32.add`, `i64.and`, ... | same | ✅ |
| `Prim`: float arithmetic | `f64.add`, `f32.sqrt`, ... | same | ✅ |
| `Prim`: comparisons | `i32.lt_s`, `f64.eq`, ... | same | ✅ |
| `Prim`: conversions | `i64.extend_i32_s`, `f64.convert_i32_s`, ... | same | ✅ |
| `Call` | `call` | `call` | ✅ |
| `Call` in tail position | `return_call` | `return_call` | ✅ (tail calls, Wasm 3.0) |
| `CallExtern` | `call` of an import | same | ✅ |
| `Closure` | allocate env, store a table index + captures | `struct.new` with `ref.func` + captures | ✅ |
| `Apply` | `call_indirect` | `call_ref` | ✅ |
| `New` | allocate, store tag + fields | `struct.new` (one struct type per constructor) | ✅ |
| `Get` | `*.load` | `struct.get` | ✅ |
| `Set` | `*.store` | `struct.set` | ✅ |
| `ArrayNew` | allocate length + elements | `array.new` | ✅ |
| `ArrayGet` | bounds check + `*.load` | `array.get` | ✅ |
| `ArraySet` | bounds check + `*.store` | `array.set` | ✅ |
| `ArrayLen` | `i32.load` of the length | `array.len` | ✅ |
| `RefNew` | allocate one slot + store, or a local if promoted | `struct.new` of a one-field mutable struct, or a local if promoted | ✅ |
| `RefGet` | `*.load` | `struct.get` | ✅ |
| `RefSet` | `*.store` | `struct.set` | ✅ |
| `Box` of a scalar | allocate + store | `ref.i31` for small integers, else `struct.new` | ✅ |
| `Box` of a reference | nothing (it's already a pointer) | nothing (upcast to `anyref`) | ✅ |
| `Unbox` to a scalar | `*.load` | `i31.get_s`, or `ref.cast` + `struct.get` | ✅ |
| `Unbox` to a reference | nothing | `ref.cast` | ✅ |
| String literal (`Lit`) | data segment | `array.new_data` | ✅ |
| `Copy` | generated copy function per type | same | ✅ |
| `Handle` | `cont.new` + `resume` with `(on $op ..)` | same | ⚠️ stack switching |
| `Perform` | `suspend` | same | ⚠️ stack switching |
| `Resume` | `resume` | same | ⚠️ stack switching |
| `Handle`, `Perform`, `Resume` | evidence passing, then the rows above | same | 🔧 |

Notes:

- `case` on an ADT reads the tag: an `i32.load` with linear memory, and
  `br_on_cast` or a tag field with Wasm GC.
- Integer division traps on zero in Wasm. If a frontend wants its own
  error instead, Conduit emits a check before `div`.
- Linear memory has no collector yet: memory is never freed for
  now. With Wasm GC, the host collects.
- Mutability doesn't exist at run time: `Imm` and `Mut` objects and
  closures have the same layout. With Wasm GC, every struct and array field
  is declared mutable, even for `Imm` objects, so that freezing in place
  doesn't change an object's Wasm type. Engines barely optimize immutable
  fields, so little is lost.
- A copy function walks the value's type, recursing for recursive types,
  and keeps a table of objects it has already copied, to preserve sharing
  and cycles.

## Left out on purpose

- **Generic code.** Frontends monomorphize, and erase to `Any` where that
  doesn't terminate. See "Polymorphism".
- **Exceptions.** They're an effect with an aborting clause, so they need
  nothing of their own.
- **Shallow handlers and multi-shot continuations.** See open questions.
- **Mutable variables.** Refs cover them, and promotion removes the cost.
- **Nested functions, unstructured gotos.** Both can be expressed with the
  constructs above.

## Open questions

1. GC: target Wasm GC (`struct.new`, `ref`) directly, or keep linear memory
   with a Conduit runtime? This decides how `Data` and `Array` lower, not
   what the IR looks like.
2. Should `Case` allow several tags per arm (or-patterns like `0 | 1`)? It's cheap
   when an arm binds nothing, but join points already cover it.
3. Multi-shot continuations (backtracking, nondeterminism): allow them
   per effect, with a flag on `EffectDecl`? This is only possible with
   the evidence-passing lowering, not with Wasm stack switching.
4. Shallow handlers: needed for anything frontends want to express, or can
   deep handlers plus state cover it?
5. Should objects carry a mutability bit at run time? `Copy` on a
   `ReadOnly` value could then skip objects that are already `Imm`, instead
   of copying everything. It costs a header bit per object.
6. Should `Copy(Imm)` follow `Mut` closures instead of rejecting them? That
   needs a copy function per closure, stored next to its wrapper, and it
   would turn a closure's captured refs into snapshots.

Decided:

- **Closure conversion** is split: the frontend lambda-lifts (captures
  first, then parameters) and Conduit does the rest. See "Closures" right
  after "Modules". If several frontends end up writing the same free-variable
  helper, Conduit can ship it as a library function without changing the
  IR.

## What frontends need

Conduit is meant to replace a frontend's own mid-level IR, so it has to
carry what such an IR usually carries. The design above doesn't cover these
yet:

- **Generic code.** Conduit is monomorphic, so a frontend with generics or
  type classes monomorphizes and resolves method calls while lowering to
  Conduit, erasing to `Any` and passing dictionaries for polymorphic
  recursion (see "Polymorphism"). Any interpreter that runs on Conduit then
  runs that code too.
- **Mutability.** A language with `imm` and `mut` views of the same data
  maps them to the mutability in Conduit's types, and its copies between
  them to `Copy`. See "Mutability".
- **Sources.** Frontends need to map code back to source, for errors and
  for limits on how long an interpreter runs. `Let` and `Jump` carry a
  `source`, which is opaque to Conduit and carried through inlining.
- **Strings and chars.** `Lit` needs string literals (lowered to data
  segments or GC arrays). Chars are `i32`.
- **Incremental compilation.** A query-based frontend (Salsa, for example)
  caches Conduit IR and wants unchanged functions not to be recompiled.
  Conduit doesn't depend on any such framework. Instead:
  - Passes are pure functions that work on one function at a time
    (`optimize`, `emit_wasm`), and only `link` sees the whole program.
  - A pass reaches other functions and data types through a trait the
    frontend implements:

    ```rust
    pub trait Program {
        fn func(&self, id: FuncId) -> Arc<Func>;
        fn data(&self, id: DataId) -> Arc<DataDecl>;
    }
    ```

    A Salsa frontend implements it with tracked queries. When inlining
    asks for a callee, Salsa records the dependency, so a change to the
    callee recompiles the caller.
  - IDs are opaque `u64`s the frontend chooses, and stay the same for the
    same item. A Salsa frontend can use the bits of an interned ID
    (`salsa::Id::as_bits`). Positions only exist after `link`.
  - Passes are deterministic: the same input gives exactly the same
    output, so a frontend's equality check can tell that nothing changed.
    Iteration order never depends on a `HashMap`.
  - A pass asks for what it actually uses, such as the callees inlining
    considers, so a function doesn't depend on the whole program.

  If several Salsa frontends need the same glue, it can live in a separate
  crate (for example `conduit-salsa`), so Conduit's core stays
  framework-free.
