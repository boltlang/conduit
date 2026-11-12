# Writing a frontend

How a compiler uses Conduit: what to build, which steps to run in which
order, and what Conduit carries that a frontend's own IR would. The IR
itself is in [ir.md](ir/ir.md).

## The pipeline

From a frontend's program to a Wasm module:

| Step | What | Why there |
|---|---|---|
| 1. Build | each function with `conduit_ir::build::FuncBuilder`, or the module as text with `conduit_ir::text::parse` | |
| 2. Export | `m.exports`: what the host calls, like `main` | before optimizing, so that the passes keep exported functions and their parameters owned |
| 3. Verify | `conduit_ir::verify::verify_module` | every pass takes verified IR, and the evaluator only runs it |
| 4. Optimize | `conduit_opt::optimize_module` | optional; on IR without counts, since the passes don't keep counts right |
| 5. Lower effects | `conduit_effects::lower_effects`, unless `conduit_ir::clause::direct_handlers` says the backend lowers the handlers itself | before counts: evidence passing makes new objects to count |
| 6. Count | `conduit_opt::ownership::insert_reuse` (optional), `insert_counts`, then `specialize_drops` (optional) | last before emitting, on the final code |
| 7. Emit | `conduit_wasm::emit_module_with(&m, &Options)`, with `counts` on if step 6 ran | |

Step 5 isn't needed with `Options::stack_switching`, which lowers any
handler in Wasm, nor when every handler's clauses are tail-resumptive or
aborting, which the Wasm backend lowers directly. Without step 6, memory is
never freed, which is fine for short programs and tests.

To run the program in the evaluator instead, stop after step 3 or 4, and
give `conduit_eval::Machine::new` a host for its externs: it runs IR with
or without counts, with `Machine::counting` on to check them.

## An example

`let x = if c { a } else { b }; x + 1`, built in the order a frontend walks
it, verified, optimized and lowered:

```rust
use conduit_ir::build::FuncBuilder;
use conduit_ir::*;

let mut m = Module::default();
let f = FuncId(0);
// The function's signature first, so that it can call itself.
m.funcs.insert(f, Func::new(vec![Ty::I32; 3], vec![Ty::I32]));

let mut b = FuncBuilder::new(&m, vec![Ty::I32; 3], vec![Ty::I32]);
let [c, x, y] = b.params();
let k = b.join(&[Ty::I32]);         // the code after the `if`, which takes x
let case = b.case(c);
b.arm(case, 1);
b.jump(&k, [x]);
b.default(case);
b.jump(&k, [y]);
b.switch_to(k.block());
let r = b.add(k.param(0), 1i32);
b.ret([r]);
m.funcs.insert(f, b.finish_verified(f).unwrap());
m.exports.insert(f, Export::Wasm { name: "pick".into() });

conduit_ir::verify::verify_module(&m).unwrap();
conduit_opt::optimize_module(&mut m);
if !conduit_ir::clause::direct_handlers(&m) {
    conduit_effects::lower_effects(&mut m).unwrap();
}
conduit_opt::ownership::insert_counts(&mut m).unwrap();
let options = conduit_wasm::Options { counts: true, ..Default::default() };
let wasm: Vec<u8> = conduit_wasm::emit_module_with(&m, &options).unwrap();
```

As text, the function is:

```
#[export(wasm, name = "pick")]
fn fn0(%0: i32, %1: i32, %2: i32) -> (i32) {
  join j1(%3: i32) =
    let %4: i32 = add(%3, 1i32)
    jump ret(%4)
  in
  case %0 {
    1 => jump j1(%1)
    _ => jump j1(%2)
  }
}
```

Bolt's `crates/boltlang-codegen-conduit` (`wasm.rs` and `run.rs`) does all of
this for a whole language.

## What Conduit carries for a frontend

Conduit is meant to replace a frontend's own mid-level IR, so it carries
what such an IR usually carries:

- **Generic code.** Conduit is monomorphic, so a frontend with generics or
  type classes monomorphizes and resolves method calls while lowering to
  Conduit, erasing to `Any` and passing dictionaries for polymorphic
  recursion (see "Polymorphism" in [ir.md](ir/ir.md)). Any interpreter that runs on Conduit then
  runs that code too.
- **Mutability.** A language with `imm` and `mut` views of the same data
  maps them to the mutability in Conduit's types, and builds its copies
  between them with `New` and `ArrayClone`. Code that's generic over
  mutability keeps its parameters, as functions with mutability
  parameters. See "Mutability" in [ir.md](ir/ir.md).
- **Sources.** Frontends need to map code back to source, for errors and
  for limits on how long an interpreter runs. `Let` and `Jump` carry a
  `source`, which is opaque to Conduit and carried through inlining.
- **Strings and chars.** A string literal is a constant of type
  `imm [i8]` (see "Constants" in [ir.md](ir/ir.md)), written `b"hi"` in the text, which a `Mut` string copies with
  `ArrayClone`. Chars are `i32`.
- **Incremental compilation.** A query-based frontend (Salsa, for example)
  caches Conduit IR and wants unchanged functions not to be recompiled.
  Conduit doesn't depend on any such framework. Instead:
  - Passes are pure functions that work on one function at a time
    (`optimize_func`, `verify_func`); only the steps that change the
    whole module, like `lower_effects`, `insert_counts` and
    `emit_module_with`, see all of it.
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
    (`salsa::Id::as_bits`). Positions only exist in what a backend emits.
  - Passes are deterministic: the same input gives exactly the same
    output, so a frontend's equality check can tell that nothing changed.
    Iteration order never depends on a `HashMap`.
  - A pass asks for what it actually uses, such as the callees inlining
    considers, so a function doesn't depend on the whole program.

  If several Salsa frontends need the same glue, it can live in a separate
  crate (for example `conduit-salsa`), so Conduit's core stays
  framework-free.
