# Conduit's documentation

| Document | What's in it | For |
|---|---|---|
| [ir.md](ir.md) | The IR: types, ops, rules, mutability, reference counts, effect handlers and their types, and why it's built the way it is | everyone |
| [frontends.md](frontends.md) | How a compiler uses Conduit: the pipeline from building IR to a Wasm module, an example, and what Conduit carries for a frontend | frontend authors |
| [text.md](text.md) | The text format that `conduit_ir::text` reads and prints, which tests and error messages use | everyone |
| [verify.md](verify.md) | What the verifier checks, rule by rule | frontend and pass authors |
| [optimisations.md](optimisations.md) | The optimizer's passes, one by one, and the few rewrites they're made of | pass authors |
| [lowerings.md](lowerings.md) | Evidence passing, which removes effect handlers, and reference counting, which makes counts explicit | frontend and backend authors |
| [wasm.md](wasm.md) | The Wasm backend: its options, how it lowers control flow and handlers, and how values live in linear memory | backend authors |
| [llvm.md](llvm.md) | The experimental LLVM backend: what it lowers, how, and its tests | backend authors |
| [host/js.md](host/js.md) | Running in browsers and Node.js: what the engine needs, host values, strings, callbacks, garbage collection | embedders and runtime authors |
| [evaluator.md](evaluator.md) | `conduit-eval`, which runs IR directly, for tests | frontend and pass authors |

The test programs, which show most features at work, are described in
[`tests/programs/README.md`](../tests/programs/README.md).

## Words

- **Join point:** a local continuation, defined by `join` and reached by
  `jump`: the code after an `if`, or a loop (`join rec`). It can't escape
  its function, so it's a jump, not a call.
- **Tail:** what ends a block: a `join`, a `jump`, a `case`, or
  `unreachable` or `trap`. A **tail call** is a call whose results the
  function only returns, which doesn't grow the stack.
- **`imm` all the way down:** a value whose every reachable object is
  `imm`, so nothing reachable from it ever changes. What a constant, an
  `imm` closure's captures and an `imm any` must be.
- **Owned and borrowed:** with explicit counts, a variable that owns a
  reference to an object drops it when it's done; one that borrows it
  doesn't, since another variable owns it for longer. A parameter is
  borrowed when the caller keeps its reference for the call.
- **Takes:** an op that takes an argument's reference moves it into what
  it makes or into the callee; one that doesn't borrows it.
- **Tail-resumptive clause:** a handler clause whose every way out resumes
  its continuation as the last thing it does. It can run as a plain call,
  with no continuation captured.
- **Aborting clause:** a clause that never uses its continuation, like an
  exception's. The handler's body is abandoned.
- **Evidence:** in evidence passing, the value each function that may
  perform takes: the handlers in scope and whether code is yielding.
- **Region:** in the `copies` pass, the variables that may reach the
  same `mut` object. An `array_clone` of an array becomes an
  `array_freeze` when the array's region doesn't escape its function and
  none of the region's variables is used after the clone.
- **Host value:** a value of the host, like a JavaScript object, that
  Conduit only holds and passes on: `host` and `nullable host`.
- **Trampoline:** a function the host calls to run a closure of a given
  signature, made for `host_function` and for stack switching.
