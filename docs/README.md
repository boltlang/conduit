# Conduit's documentation

Start with [frontends.md](frontends.md), which shows the pipeline from
building IR to running it, with an example. The rest is grouped by what
it's about.

## The IR: [`ir/`](ir/)

| Document | What's in it | For |
|---|---|---|
| [ir.md](ir/ir.md) | The IR: types, ops, rules, mutability, reference counts, and why it's built the way it is | everyone |
| [effects.md](ir/effects.md) | Effect handlers, continuations and effect types: rows and effect parameters | everyone |
| [exceptions.md](ir/exceptions.md) | Exceptions the host throws and Conduit catches or throws: the `throw` effect and its rules | frontend, backend and runtime authors |
| [text.md](ir/text.md) | The text format that `conduit_ir::text` reads and prints, which tests and error messages use | everyone |
| [verify.md](ir/verify.md) | What the verifier checks, rule by rule | frontend and pass authors |

## Passes: [`passes/`](passes/)

| Document | What's in it | For |
|---|---|---|
| [optimisations.md](passes/optimisations.md) | The optimizer's passes, one by one, and the few rewrites they're made of | pass authors |
| [lowerings.md](passes/lowerings.md) | Evidence passing, which removes effect handlers, and reference counting, which makes counts explicit | frontend and backend authors |

## Backends: [`backends/`](backends/)

| Document | What's in it | For |
|---|---|---|
| [wasm.md](backends/wasm.md) | The Wasm backend: its options, how it lowers control flow and handlers, and how values live in linear memory | backend authors |
| [llvm.md](backends/llvm.md) | The experimental LLVM backend: what it lowers, how, and its tests | backend authors |
| [evaluator.md](backends/evaluator.md) | `conduit-eval`, which runs IR directly, for tests | frontend and pass authors |

## Hosts: [`host/`](host/)

What the code around a module must do to run it.

| Document | What's in it | For |
|---|---|---|
| [js.md](host/js.md) | Running in browsers and Node.js: what the engine needs, host values, strings, callbacks, garbage collection, exceptions | embedders and runtime authors |
| [native.md](host/native.md) | Running natively, through the LLVM backend: building, linking, the C calling convention, host values, callbacks, exceptions, with an example | embedders and runtime authors |

## Internals: [`internals/`](internals/)

How parts of Conduit work inside, for those who change them.

| Document | What's in it |
|---|---|
| [wasm-unwinding.md](internals/wasm-unwinding.md) | How the Wasm backend keeps a module's state right when an exception or a trap unwinds through it |

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
