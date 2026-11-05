# Conduit IR (draft)

Conduit's IR is a typed, monomorphic, A-normal-form tree with join points,
with a boxed `Any` type for code that can't be monomorphized.
It is meant to be small: a pass handles **5 kinds of thing**, a `let` and
the 4 kinds of tail that end a block. Everything that isn't control flow is a
single `let` holding an operation, so a pass that doesn't care about a
particular operation never has to match on it.

## Goals

- **Few cases.** A block is a list of `let`s and one of 4 tails. Operands
  have 2 kinds. Every operation has the same shape, `let xs = op(args)`.
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
- **Handlers add no control flow.** Effect handlers are three more `Op`s and one
  more type. Passes that ignore them keep working unchanged.

## The whole IR

```rust
/// An operand: never a nested expression.
pub enum Atom {
    Var(Var),
    Lit(Lit),          // integer or float constant
}

/// Straight-line code, then what ends it. The `let`s are a list, so a long
/// function is a long `Vec`, not a deep chain: dropping, cloning, comparing
/// or walking one never recurses per statement. Blocks only nest through
/// `join` and `case`.
pub struct Block { lets: Vec<Let>, tail: Tail }

/// `let x1, .., xn = op(a1, .., am)`
pub struct Let { binds: Vec<Var>, op: Op, args: Vec<Atom>, source: Option<SourceId> }

pub enum Tail {
    /// `join [rec] j1(params) = b1, .., jn(params) = bn in body`
    Join { rec: bool, defs: Vec<JoinDef>, body: Box<Block> },

    /// `jump j(a1, .., an)`, which also covers return: see "Returning" below.
    Jump { target: Label, args: Vec<Atom>, source: Option<SourceId> },

    /// Branch on an integer or on the constructor of an ADT value.
    /// `default` runs when no arm's tag matches.
    Case { scrut: Atom, arms: Vec<Arm>, default: Option<Box<Block>> },

    /// This point is never reached: the frontend promises it, so reaching
    /// it is undefined behavior, and passes may assume it doesn't happen
    /// (after a call to a `!` function, an arm the type checker rules out).
    Unreachable,

    /// Stops the program with an error (an abort, a failed match the
    /// frontend reports). Reaching it is defined, so passes keep it.
    Trap,
}

pub struct JoinDef { label: Label, params: Vec<Var>, body: Block }

pub struct Arm { tag: Tag, body: Block }

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
    Call(FuncId, Vec<Mutability>),     // direct call, with mutability arguments
    CallExtern(ExternId),  // host function (FFI, runtime builtins)
    Closure(FuncId, Vec<Mutability>),  // args = captured values; result has type Fn(sig)
    Apply,                 // args[0] = closure, rest = arguments
    New(CtorId),           // allocate an ADT value; args = its fields
    Const(ConstId),        // a constant's object, the same every time; no args
    Undefined,             // a value of the type it binds that must not be used; no args
    Get(CtorId, u32),      // read field i
    Set(CtorId, u32),      // write field i of a Mut object; binds nothing
    ArrayNew,              // args = the elements
    ArrayFill,             // args = a length, then the value to repeat
    ArrayAlloc,            // args = a length; a Mut array whose elements aren't set yet
    ArrayCopy,             // args = dst, dst start, src, src start, count; binds nothing
    ArrayGet, ArraySet, ArrayLen,   // ArraySet: Mut arrays only
    ArrayClone,            // args = an array; a new one with the same elements: see "Mutability"
    ArrayFreeze,           // args = an array; `ArrayClone` to `Imm` done in place
    RefNew,                // args = initial value; result has type Ref(ty)
    RefGet,                // args = the ref
    RefSet,                // args = the ref, then the new value; binds nothing
    Box(Ty),               // args = a value of that type; result has type Any(_)
    Unbox(Ty),             // args = an Any(_); result has that type
    Handle(HandlerId),     // args = handler env, then a closure: the body
    Perform(EffectId, u32),// run operation i of the nearest handler of the effect
    Resume,                // args[0] = continuation, rest = values to resume with
    Discontinue(EffectId, u32), // args[0] = continuation, rest = the operation's args
}

impl Op {
    fn effects(&self) -> Effects;          // memory: None | Reads | Writes | Unknown, and may_trap
    fn result_types(&self, ..) -> Vec<Ty>;
}
```

Adding an operation means adding an `Op` variant and its `effects()`. No
control-flow pass needs to change.

An op's effects say two things: what it may do to memory that existed
before it (nothing, read, write, or anything, for ops that run code), and
whether it may trap. They're separate since passes ask different things:
removing an unused op needs it not to write or trap, but two ops with the
same arguments can be one if nothing writes in between, even if they may
trap, since the second can't trap if the first didn't. Ops that may trap
are signed integer division (the smallest value by -1), float to integer
conversions (out of range), array accesses and `ArrayCopy` (out of
bounds), `ArrayFill` and `ArrayAlloc` with a length that's negative or
too big, `Unbox` (a failed cast), and every op that runs code.

**Arrays whose elements aren't set yet.** `ArrayAlloc(n)` makes a `Mut`
array of `n` elements that aren't set, for code that keeps track of which
ones it has set, like a growable array's spare room. Reading one before
it's set is undefined behavior, as reading uninitialized memory is in
LLVM. That's the rule every backend can follow for free: Wasm GC's
`array.new_default` gives null or 0, a native backend's collector needs
zeroed memory anyway, and a data type has no value Conduit could give
instead. The evaluator stops at such a read. `ArrayCopy` copies a range
of one array into a `Mut` one, as `memmove` does: as through a
temporary, so the ranges can overlap. Unset elements stay unset, so a
growable array can move its elements to a bigger array with `ArrayAlloc`
and `ArrayCopy`. It traps, as Wasm's `array.copy` does, if either range
doesn't fit in its array, its starts and count read as unsigned, so a
negative one always does; copying nothing at the end is fine.

Integer division and remainder by zero don't trap: `x / 0` is 0 and
`x % 0` is `x`, so `(x / y) * y + x % y` is still `x`. A frontend whose
language traps or raises an error there checks the divisor itself. An op
that can't trap can be removed or moved, and a check the frontend wrote
is visible to the optimizer, which a hidden trap isn't.

### Types

```rust
pub enum Ty {
    I8, I16, I32, I64, F32, F64,
    Data(DataId, Mutability, Vec<Mutability>),  // GC'd ADT value, with its arguments
    Array(Box<Ty>, Mutability),   // GC'd array
    Fn(Box<Sig>, Mutability),     // closure
    Cont(Box<Sig>),               // one-shot continuation captured by a handler
    Ref(Box<Ty>),                 // mutable cell holding one value: always Mut
    Any(Mutability),              // a boxed value of any type: see "Polymorphism"
}

pub enum Mutability { Imm, Mut, ReadOnly, Inherit, Param(u32) }

impl Ty {
    fn mutability(&self) -> Option<Mutability>;   // None for scalars and Cont
    fn with_mutability(&self, m: Mutability) -> Ty;
}

pub struct DataDecl { params: u32, ctors: Vec<CtorDecl> }
pub struct CtorDecl { fields: Vec<Ty> }
```

Mutability is part of the types that are objects: see "Mutability".
`Inherit` is only valid in the field types of a `CtorDecl` and in the
element type of an `Array`, and `Param(i)` only in the field types of a
`DataDecl` with at least `i` parameters and in the types of a `Func` with
at least `i`. A `Data` type gives exactly one argument per parameter of
its declaration: `Imm`, `Mut`, `ReadOnly` or a parameter of the function
it's in.

Integers are `i8`, `i16`, `i32` and `i64`. As in LLVM they have no sign:
an op that reads one as signed or unsigned says so (`div.s`, `lt.u`,
`convert.u[i32]`), and the others wrap. So a byte is an `i8`, and a string
an `imm [i8]`. `convert` to a narrower integer keeps the low bits, and to
a wider one extends as its sign says: `convert.u[i32](255i8)` is `255i32`,
`convert.s[i32](255i8)` is `-1i32`. Literals can be written either way:
`255i8` is `-1i8`. `case` tags are read as signed, so a `case` on an `i8`
has tags from `-128` to `127`.

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
    consts: Vec<ConstDecl>,
}

pub struct Func {
    mut_params: MutParams, // see "Mutability parameters on functions"
    params: Vec<Var>,
    results: Vec<Ty>,
    body: Block,           // `ret` is in scope: see below
}

pub struct MutParams {
    count: u32,
    assumes: Vec<(Mutability, Mutability)>,  // (a, b): a is at most b
}
```

Functions exist only at the top level.

### Constants

```rust
pub struct ConstDecl { ty: Ty, value: ConstValue }

pub enum ConstValue {
    Lit(Lit),
    Array(Vec<ConstValue>),          // an imm array
    New(CtorId, Vec<ConstValue>),    // an imm object
    Const(ConstId),                  // the same object as another constant
}
```

A constant is a value that exists before the program runs, like a global
constant in LLVM. `Const(c)` gives its object, and every use gets the same
one. Its type must be `Imm` all the way down, so that nothing can tell
that it's shared, and so that an optimization may treat it as any other
`Imm` value. A string is a constant of type `imm [i8]`; a table, a
constant array; a list, nested `New`s.

The value is a tree of literals, arrays and constructors, whose types
come from the constant's type, as an array's elements come from its
element type. `Const` lets constants share parts, and cut a value too deep
to be one tree into several. They can't refer to each other in a cycle.

A frontend needing a `Mut` value with the same contents builds one from a
constant, as C copies a string literal into an array: `ArrayClone` for an
array, `New` for an object.

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
is known. (They can have mutability parameters, which don't change how
anything is laid out: see "Mutability parameters on functions".) Frontends monomorphize, which gives fast, unboxed code. That
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

`Unbox(ty)` on a value that wasn't boxed at a subtype of `ty` is
**undefined behavior**. The verifier can't check it: the frontend's type
checker has already proven it can't happen. Backends may do anything:
Wasm GC traps, and linear memory doesn't check. The evaluator reports it
as an error.
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
- **`Inherit`** means "the mutability of the object holding this": in a
  `CtorDecl`'s field types, of the data object, and in the element type
  of an array, anywhere, of the array. A `Get` on a `Data(D, m)` gives a
  field of type `Data(E, Inherit)` the type `Data(E, m)`, and an
  `ArrayGet` on an `Array(Data(E, Inherit), m)` gives `Data(E, m)`.
  Fields and elements with an explicit mutability keep it. An `Inherit`
  always refers to the nearest data object or array around it, so in a
  field of type `Array(Data(E, Inherit), Inherit)` the first is the
  array's and the second the object's.

  `Inherit` says that what it's on belongs to the object holding it,
  while an explicit mutability says it's shared:

  Seen as `ReadOnly`, an `Array(Data(E, Inherit), Mut)` holds `ReadOnly`
  elements, so the view is deep. An `Array(Data(E, Mut), Mut)` seen as
  `ReadOnly` still holds `Mut` ones.

- **`Param(i)`**, a mutability parameter, is chosen by each data type of
  the declaration. `data D['m1] { ('m1 [i32]) }` is used as `mut D[imm]`
  (a `mut` object holding an `imm` array), `mut D[mut]` or `imm D[imm]`,
  all of one declaration. A `Get` on a `Data(D, m, args)` gives a field the
  type with `args[i - 1]` in place of `Param(i)` and `m` in place of
  `Inherit`. `Inherit` is in effect parameter 0: every data type and array
  has it, and it's always the object's own mutability. A declared
  parameter is the same without that tie, which is what lets a value's
  parts have mutabilities other than its own.

An object of mutability `m` holds only what is **at most** `m` all the
way down, in the order `Imm ⊑ ReadOnly ⊑ Mut`. So an `Imm` one holds no
`Ref`, `Cont` or anything `Mut` or `ReadOnly`, and a `Mut` one holds
anything. Only `Mut` objects can be written. `Imm` and `Mut` are
subtypes of `ReadOnly`, and nothing else converts between mutabilities: a
value of another mutability is a new value (see below). The verifier
checks all of this: see "Mutability" in [verify.md](verify.md).

### Mutability parameters on functions

A function can have mutability parameters, so that one function serves
callers of either mutability without losing what they know.
`ReadOnly` covers a parameter that's only read, but not a result: a
function that gives back what it gets would give a `Mut` caller a value it
can't write, and an `Imm` caller one it can't put in an `Imm` object. And it
can't make objects at all, since no object is made `ReadOnly`.

```
fn fn0['m1](%0: 'm1 [i32]) -> ('m1 [i32]) { jump ret(%0) }
fn fn1['m1](%0: i32) -> ('m1 [i32]) { .. array_fill(%0, 0i32) .. }

let %1: mut [i32] = call fn0[mut](%0)    // still mut
let %2: imm [i32] = call fn1[imm](3i32)  // made imm
```

- **Each call gives an argument per parameter**, as does each `Closure`:
  `Imm`, `Mut` or a parameter of the caller, never `ReadOnly`. So inside
  the function, a parameter is `Imm` or `Mut`, not known which. The
  callee's signature is seen with the arguments in place of its
  parameters. A handler's clauses have no parameters, since nothing gives
  them arguments.
- **A parameter can't be written:** `Set` and `ArraySet` need `Mut`, and a
  parameter may be `Imm`. It can be read, seen as `ReadOnly`, and made:
  `New`, `ArrayNew`, `ArrayFill`, `ArrayClone` and `Closure` can bind a
  type of a parameter's mutability.
- **Holding** follows "at most": a `'m1` object can hold scalars, `Imm`
  values and `'m1` ones, since all of them are at most `'m1` whichever it
  is, but not a `Mut` one, nor one of another parameter `'m2`, unless the
  function assumes it.
- **Assumptions** say more: `where 'm2: 'm1` assumes `'m2 ⊑ 'm1`, so a
  `'m1` object can hold a `'m2` one. `where 'm1: imm` makes `'m1` `Imm`.
  Each call must meet them with its arguments, under the caller's own
  assumptions: `fn2['m1, 'm2]` with `'m2: 'm1` can be called as
  `fn2[mut, imm]` or `fn2[imm, imm]`, but not `fn2[imm, mut]`, which would
  put a `Mut` array in an `Imm` box. Assumptions are about holding, not
  subtyping: a `'m1` value never fits where `Imm` is expected, whatever is
  assumed.

A frontend that monomorphizes can keep a generic function's `'m`
parameters and `where` clauses, specializing an instance on types but not
on mutabilities. At run time nothing changes: mutability doesn't exist in
a backend, so one function is compiled once for every mutability it's
called with. Only the evaluator, whose objects know their mutability,
carries the arguments of each call.

### Copying between mutabilities

A `Mut` value can't become `Imm` in place, since a `Mut` reference to it
may still exist. A value of another mutability is a new value, which the
frontend builds: a data object with `New`, from the original's fields,
and an array with **`ArrayClone`**.

`ArrayClone` makes a new array of the mutability the variable it binds
says, with the original's elements, which it shares. So the elements must
fit the new array: an `Imm` clone needs elements that are `Imm` all the
way down, and an `Array(Data(E, Inherit), Mut)` clones to `Imm` only if
its elements are rebuilt first. `ArrayClone` is the one conversion a
frontend can't write itself: `ArrayNew` at `Imm` needs every element up
front, so an `Imm` array whose elements are computed, like the buffer of
a growing array, needs a `Mut` one cloned. A deeper copy, of a value and
what it holds, is the frontend's to write, type by type, which also lets
it handle the sharing and cycles its own data structures have.

**`ArrayFreeze`** gives what an `ArrayClone` to `Imm` gives, with the same
types, but makes the array itself `Imm` instead of making a new one. It is
only allowed when nothing but its result can reach the array afterwards:
no other variable, object, closure, ref or `Any` that's still used. The
verifier can't check this, so breaking it is **undefined behavior**, like
`Unbox` at the wrong type. Optimizations make `ArrayFreeze` from an
`ArrayClone` they show is the only way left to the array (see "Remove
copies"), and a frontend can emit it where it already knows an array is
unique. Its effect is `Reads`, like `ArrayClone`'s: it changes an array
that nothing else can see.

`ArrayClone` is an op rather than a loop the frontend writes because
removing copies is the main optimization (see "Optimizations"), and an
analysis can only remove what it can recognize. Its effect is `Reads`: it
allocates, but it writes nothing that existed before.

## Rules

The verifier checks these rules, and the types of every op: see
[verify.md](verify.md).

1. **Variables are SSA.** Each one is bound once, by a function parameter, a
   join parameter or a `let`. It is in scope in the rest of its block (the
   later `let`s and the tail) and in everything nested in it.
2. **Labels are scoped.** A non-recursive `Join` puts its labels in scope
   in `body`. A recursive one also puts them in scope in every `defs[i].body`.
3. **Jumps are tail calls by construction.** `Jump` is a `Tail`, and a
   tail can only end a block. There is no `let x = case ...`. Nothing needs
   to check for escaping join points, and no pass can accidentally make one.
4. **Returning is jumping.** Every function body has the label `ret` in
   scope, whose parameters are the function's results. So return needs no
   tail of its own. A tail call is just `let r = call f(..); jump ret(r)`,
   which a backend can recognize and emit as `return_call`.
5. **Field reads happen where they're written.** A `Case` on an ADT binds
   no fields. `Get(C, i)` on `x` is valid in an arm `C` of a case on `x`,
   after `x` is bound by `New(C)`, or when `x`'s type has a single
   constructor. Fields aren't bound in the
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

### The four operations

```
let r = handle H(env.., body)   -- run closure `body` under handler H
let x = perform E.op(args)      -- nearest enclosing handler of E (dynamic)
let r = resume(k, vals)         -- continue a captured computation
let r = discontinue F.op(k, args) -- continue it by performing F.op(args) there
```

`discontinue` continues `k` as if the `perform` it captured had been
`perform F.op(args)` instead, under the handlers that were around it, so
what `F.op` gives is what that `perform` gives. Its results must fit what
`k` takes. Like OCaml's `discontinue`, this is how a clause stops a
computation without dropping it: with an effect whose handlers clean up
and abort, a cancelled task unwinds through its own cleanup. The handlers
inside the task can also recover, by resuming, as from an error. Conduit
has no exceptions of its own, so the frontend says which effect it is.

### Why the body is a closure and not a block

A handler could be a 5th kind of tail with a nested body, but then a jump in the
body could target a label outside it. Once `perform` has captured the body
as a continuation, that jump would run code that's outside what was
captured. Making the body a function keeps rule 2 unchanged: labels never
cross a function boundary, so they never cross a handler boundary either.
Join points stay jumps, even inside handled code.

### Rules

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

### Effect types

Rows on functions, closure types, continuation types and handlers, and
effect parameters on functions, data types and handlers, are
implemented, and the verifier checks them. A frontend that doesn't track
effects writes `does Any`. `cse` and `handlers` use rows; the other
passes still assume that every call and `apply` can perform.

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
  (`fn(..) -> (..) does ..`) and a continuation type's. An extern's and an
  effect operation's have none. A continuation's row is that of its
  handler, since resuming it runs the rest of the body, which is still
  handled.
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
  `handle` takes its effect out of the body's row: the body can perform
  `E` and what the handler's row allows, and the `handle` performs only the
  handler's row.

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
becomes one effect per instance, as a generic data type does.

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
  nothing that existed before (see "`Imm` closures don't write").

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
- Operations with type parameters of their own, like a `raise` that can
  give any type: erase the type to `Any`, or make one operation per
  instance.
- Externs that suspend, through JSPI, which would need extern rows.
- Whether handler clauses can also have mutability parameters, now that
  `handle` gives arguments.

#### Why rows are sets

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

### Lowering

The Wasm backend lowers handlers whose clauses are all tail-resumptive or
aborting without capturing a continuation:

- A global holds a stack of handler frames. A frame holds its effect's
  number, the next frame, a reference to each clause, and the handler's
  environment.
- `handle` pushes a frame, applies the body, pops the frame, and calls the
  return clause with the body's results.
- `perform` walks the stack to the nearest frame of its effect, trapping
  if there's none. It then calls that frame's clause directly, with the
  stack set to the frames below while the clause runs, so that a
  `perform` in the clause goes to an outer handler, as it would with a
  captured continuation.
- An aborting clause's wrapper stores what the clause gives in the frame
  and throws a Wasm exception holding the frame. A handler with such a
  clause runs its body in a `try_table`, which catches the exception,
  rethrows it if the frame isn't its own, and otherwise gives the frame's
  results.
- A tail-resumptive clause that discontinues sets the stack to that of
  the `perform` it handles, which a second global holds while the clause
  runs, and performs. A `perform` sets that global and puts it back after,
  and a `handle` that catches an abort puts it back too, since the
  `perform`s the exception left couldn't.

A handler with any other clause needs its continuations captured. There
are two ways to do that, both possible because handlers are confined to
three ops.

- **Wasm stack switching** (the typed-continuations proposal), which the
  Wasm backend does. Each operation of an effect such a handler handles
  has a control tag. `handle` makes a frame of the handler's own kind and
  calls a function that links it onto the handler stack and runs the body
  as a Wasm continuation, with a `resume` that has a label per operation.
  `perform` finds the nearest frame of its effect as for any handler, and
  if it's of this kind, `suspend`s with the operation's tag, putting its
  handler stack back once it's resumed. The `resume` then calls the clause,
  under the handlers outside, with a Conduit continuation: a closure that
  resumes the Wasm continuation under the handler again, which makes the
  handler deep. The frame's link to the handlers outside is set each time,
  so a continuation resumed somewhere else sees the handlers there. A
  Wasm continuation can only be resumed once, so a second `resume` traps,
  as rule 7 says. `discontinue F.op(k, args)` throws a closure that
  performs `F.op(args)` into `k` with `resume_throw`, and the `perform`
  that `k` was captured at catches it and calls it. Handlers whose clauses
  are all tail-resumptive or aborting keep the lowering above, and only a
  module with a handler of this kind needs the proposal, which isn't
  standard yet: Wasmtime has it on Linux and macOS on x86-64.
- **Evidence passing** (Xie & Leijen, "Generalized evidence passing for
  effect handlers"), which `conduit_opt::effects::lower_effects` does: a
  Conduit-to-Conduit pass over the whole module that removes the four
  ops, rows and continuation types. Functions that may perform, or that
  apply closures, handle or call such functions, take evidence: the
  active handlers and a shared yield state. Tail-resumptive clauses are
  called directly; any other clause is yielded to, and the code after each
  call that may yield, which rows tell, becomes a function that the
  continuation calls (Koka's "yield bubbling"). Each continuation carries
  a "used" flag, so a second resume traps, as with stack switching. The
  output is ordinary Conduit IR, so every backend runs it, including Wasm
  without stack switching and native ones. See "Evidence passing" in
  [Optimizations](optimisations.md).

## Optimizations come down to a few rewrites

[Optimizations](optimisations.md) lists the passes one by one. Because the
IR is so small, the classic simplifications are local rewrites:

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
  so that inlining sees the calls it makes in the same round.
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

Wasm has no `i8` or `i16` values, so, as in LLVM's Wasm backend, each is
an `i32` whose low 8 or 16 bits are the value, and whose high bits can be
anything. Ops whose low bits only depend on their operands' low bits
(`add`, `sub`, `mul`, `and`, `or`, `xor`, `shl`, `neg`) are the `i32` op.
The others extend their operands first, with `i32.extend8_s`/`extend16_s`
or an `i32.and` with a mask: `div`, `rem`, `shr`, comparisons, `convert`
to a wider type, and `case`. A signed `div` traps when the result doesn't
fit, as `i32.div_s` does, and so does `convert` from a float out of range.
Arrays of them are packed, `(array (mut i8))` and `(array (mut i16))`,
and so are fields of data types, `(field (mut i8))`, which store only the
low bits. A host sees an `i8` or `i16` as an `i32`,
and should only read its low bits.

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
| `Get` | `*.load` | `struct.get`, or `struct.get_s` for `i8` and `i16` | ✅ |
| `Set` | `*.store` | `struct.set` | ✅ |
| `ArrayNew` | allocate length + elements | `array.new` | ✅ |
| `ArrayGet` | bounds check + `*.load` | `array.get`, or `array.get_s` for `i8` and `i16` | ✅ |
| `ArraySet` | bounds check + `*.store` | `array.set` | ✅ |
| `ArrayLen` | `i32.load` of the length | `array.len` | ✅ |
| `ArrayAlloc` | allocate | `array.new_default` | ✅ |
| `ArrayCopy` | bounds checks + `memory.copy` | `array.copy` | ✅ |
| `ArrayClone` | allocate + `memory.copy` | `array.new_default` + `array.copy` | ✅ |
| `RefNew` | allocate one slot + store, or a local if promoted | `struct.new` of a one-field mutable struct, or a local if promoted | ✅ |
| `RefGet` | `*.load` | `struct.get` | ✅ |
| `RefSet` | `*.store` | `struct.set` | ✅ |
| `Box` of a scalar | allocate + store | `ref.i31` for small integers, else `struct.new` | ✅ |
| `Box` of a reference | nothing (it's already a pointer) | nothing (upcast to `anyref`) | ✅ |
| `Unbox` to a scalar | `*.load` | `i31.get_s`, or `ref.cast` + `struct.get` | ✅ |
| `Unbox` to a reference | nothing | `ref.cast` | ✅ |
| `Const` | a pointer into a data segment | `global.get` of an immutable global, made by `struct.new` and `array.new_fixed` in its initializer | ✅ |
| `ArrayFreeze` | nothing | nothing | ✅ |
| `Handle`, `Perform` with tail-resumptive clauses | — | a handler stack in a global, and a direct call of the clause | ✅ |
| `Handle`, `Perform` with aborting clauses | — | the same, then `throw`, caught by `try_table` | ✅ (exception handling, Wasm 3.0) |
| `Discontinue` in a tail-resumptive clause | — | a `perform` under the handler stack of the `perform` the clause handles, kept in a second global | ✅ |
| `Handle` | `cont.new` + `resume` with `(on $op ..)` | same | ✅ (stack switching, not standard yet) |
| `Perform` | `suspend` | same | ✅ (stack switching, not standard yet) |
| `Resume` | `resume` | same | ✅ (stack switching, not standard yet) |
| `Discontinue` | `resume_throw` of a closure that performs the operation, which the `suspend` catches and calls | same | ✅ (stack switching, not standard yet) |
| `Handle`, `Perform`, `Resume`, `Discontinue` | evidence passing, then the rows above | same | 🔧 |

Notes:

- `case` on an ADT reads the tag: an `i32.load` with linear memory, and
  `br_on_cast` or a tag field with Wasm GC.
- A constant's global is made when the module is instantiated. Its
  arrays are made with `array.new_fixed`, which engines limit: V8 to
  10,000 elements. Longer arrays will need `array.new_data` from a data
  segment, which a global's initializer can't use, so they'd be made at
  startup instead.
- Integer division and remainder trap on zero in Wasm, so `div` and `rem`
  check the divisor first and give 0 or the dividend, unless it's a
  literal.
- Linear memory has no collector yet: memory is never freed for
  now. With Wasm GC, the host collects.
- Mutability doesn't exist at run time: `Imm` and `Mut` objects and
  closures have the same layout. With Wasm GC, every struct and array field
  is declared mutable, even for `Imm` objects, so that freezing in place
  doesn't change an object's Wasm type. Engines barely optimize immutable
  fields, so little is lost.

## Text format

`conduit_ir::text` reads (`parse`) and writes (`print`) a text form of a
module. Printing then parsing gives back the same module, and text that
`print` wrote parses and prints back unchanged. Without names, things are
written by number, as the verifier's messages write them: `fn1`, `data10`,
`data10.1` (constructor 1), `extern1`, `effect20`, `handler30`, `const40`,
`%3`, `j1` and `ret`.

**Names.** A function, data type, extern, effect, handler or constant can
have a name instead (`debug.name` of a `Func`, `debug_name` of the
others), and so can a function's variables and labels (`debug.vars`, `debug.labels`):
`fn sum`, `imm List`, `List.1`, `perform Yield.0`, `%acc`, `jump loop`.
They're for showing code. Passes don't look at them, but keep them, and
what a pass makes has none.

- A name without a number gets the smallest number of its kind that
  nothing else is written with: in the order they're defined for items,
  from 0, and in the order they first appear in their function for
  variables (from 0) and labels (from 1).
- `name#3` gives both, for a name that would get another number, or that
  more than one thing has, as an inlined function's variables or the
  instances of one generic function do. `print` writes the name alone
  wherever it reads back to the same number, and `name#3` everywhere else.
- Only identifiers can be names, and not the words the format uses where a
  name can be, like `ret`, `rec` or `Any`. `print` writes a thing whose
  name can't be one by its number.

```
data data11 {
  ()
  (i32, inherit data11)
}

extern extern1 "env" "print" (i32) -> ()

effect effect20 {
  (i32) -> ()
}

handler handler30 for effect20 env(ref i32) ops(fn4) ret fn5

const const40: imm [i8] = b"hi\n"
const const41: imm data11 = data11.1(1i32, data11.0())

fn fn1(%0: i32) -> (i32) {
  join rec j1(%1: i32, %2: i32) =
    let %3: i32 = ge.s(%1, %0)
    case %3 {
      1 => jump ret(%2)
      _ =>
        let %4: i32 = add(%1, 1i32)
        let %5: i32 = add(%2, %1) @src7
        jump j1(%4, %5)
    }
  in jump j1(0i32, 0i32)
}
```

- **Types** are written as the verifier prints them: `i32`, `mut data10`,
  `imm [i32]`, `ro fn(i32) -> ()`, `cont() -> ()`, `ref i32`, `imm any`.
  A closure or continuation type with effects has them after its
  results, `imm fn() -> (i32) does effect20 + effect21`, and so does a
  function, `fn fn1() -> (i32) does effect20 {`, and a handler,
  `handler handler30 for effect20 does effect21 env(..) ..`. Without
  `does` there are none, and `does Any` is anything.
  Mutabilities are `imm`, `mut`, `ro` and `inherit`. A data type gives its
  mutability arguments in brackets, `mut data10[imm, mut]`, and a
  declaration names its parameters the same way:
  `data data10['m1, 'm2] { ('m1 [i32]) }`. A function declares them the
  same way, with its assumptions after its results:
  `fn fn1['m1, 'm2](%0: 'm2 [i32]) -> ('m1 data10['m2]) where 'm2: 'm1 {`.
  Calls and closures give their arguments the same way:
  `call fn1[mut, imm](%0)`, `closure fn2['m1](..)`. Any `'name` parses, and
  parameters print as `'m1`, `'m2`, .. by position. Brackets after a data
  ID never clash with an array type, whose `[` always follows a
  mutability.
- **Binders carry types:** function and join parameters, and the variables
  a `let` binds (`let %3: i32 = ..`; `let = ..` binds nothing). The header
  of a function can add `vars(%5: i32)` for variables nothing binds, and
  `labels N` when the function has more labels than its body defines.
- **Ops** are a name, what the op takes in its name, and the arguments:
  `add`, `div.s`, `lt.u`, `convert.s[i64]`, `reinterpret[f32]`,
  `call fn2(..)`, `call_extern extern1(..)`, `closure fn2(..)`, `apply(..)`,
  `new data10.0(..)`, `const const40()`, `undefined()`, `get data10.0.1(..)`,
  `set data10.0.1(..)`,
  `array_new`, `array_fill`, `array_alloc`, `array_get`, `array_set`,
  `array_len`, `array_copy`,
  `array_clone`, `array_freeze`,
  `ref_new`, `ref_get`, `ref_set`, `box[i32](..)`, `unbox[i32](..)`,
  `handle handler30(..)`, `perform effect20.0(..)`, `resume(..)` and
  `discontinue effect20.0(..)`.
- **Literals** carry their type: `5i32`, `-1i64`, `1.5f32`, `1e300f64`.
  Finite floats are printed in the shortest form that reads back exactly.
  NaNs and infinities are written by their bits: `f32_bits(2143289344)`.
- **Constants** give their type, then their value: a literal, an array
  `[1i32, 2i32]`, a constructor with its fields `data11.1(1i32, data11.0())`,
  or another constant by its ID or name. A byte string `b"hi\n"` is an
  array of `i8` literals, and `print` writes every non-empty one that way.
  Its escapes are `\"`, `\\`, `\n`, `\t`, `\r`, `\0` and `\xff`, and other
  characters are their UTF-8 bytes. `print` writes valid UTF-8 as its
  characters, except control characters, and every other byte as `\xff`.
  It's only a way of writing: a constant holds a byte string as an array
  of literals, so passes see one form.
- **Sources** follow a `let` or a `jump`: `@src7`.
- `case` arms are `1 =>`, `data10.1 =>` or `_ =>` for the default.
  `join` definitions are separated by `and`, and the body follows `in`.
  A block can end in `unreachable` or `trap`.
- Line comments start with `//`.

A long function is a long list of `let`s, so parsing and printing it
doesn't need a deep stack.

## Evaluator

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
  finds the frames gone, which is an error (rule 7).
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
- **Objects are never freed** yet.

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

Decided:

- **No deep copy.** Conduit used to have a `Copy` op that converted a whole
  value to another mutability, keeping sharing and cycles. Compiling that
  needs a table of copied objects, which Wasm can't key by identity, and a
  frontend that knows its data structures can copy them for less, type by
  type. So copies are the frontend's, built with `New` and `ArrayClone`.
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
  maps them to the mutability in Conduit's types, and builds its copies
  between them with `New` and `ArrayClone`. Code that's generic over
  mutability keeps its parameters, as functions with mutability
  parameters. See "Mutability".
- **Sources.** Frontends need to map code back to source, for errors and
  for limits on how long an interpreter runs. `Let` and `Jump` carry a
  `source`, which is opaque to Conduit and carried through inlining.
- **Strings and chars.** A string literal is a constant of type
  `imm [i8]` (see "Constants"), written `b"hi"` in the text, which a `Mut` string copies with
  `ArrayClone`. Chars are `i32`.
- **Incremental compilation.** A query-based frontend (Salsa, for example)
  caches Conduit IR and wants unchanged functions not to be recompiled.
  Conduit doesn't depend on any such framework. Instead:
  - Passes are pure functions that work on one function at a time
    (`optimize`, `emit_wasm`), and only `link` sees the whole program.
  - A pass reaches other functions and data types through a trait the
    frontend implements:

    ```rust
    pub trait Program {
        fn func(&self, id: FuncId) -> Option<&Func>;
        fn func_sig(&self, id: FuncId) -> Option<Sig>;   // has a default
        fn func_mut_params(&self, id: FuncId) -> Option<MutParams>;  // has a default
        fn func_effect_params(&self, id: FuncId) -> Option<u32>;     // has a default
        fn data(&self, id: DataId) -> Option<&DataDecl>;
        fn extern_fn(&self, id: ExternId) -> Option<&Extern>;
        fn effect(&self, id: EffectId) -> Option<&EffectDecl>;
        fn handler(&self, id: HandlerId) -> Option<&HandlerDecl>;
        fn constant(&self, id: ConstId) -> Option<&ConstDecl>;
    }
    ```

    A Salsa frontend implements it with tracked queries. When inlining
    asks for a callee, Salsa records the dependency, so a change to the
    callee recompiles the caller. A pass that only needs a callee's type,
    like the verifier, asks for `func_sig`, so that it doesn't depend on
    the callee's body.
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
