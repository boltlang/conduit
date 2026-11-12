# Running Conduit in JavaScript: browsers and Node.js

How a Wasm module from `conduit-wasm` talks to a JavaScript host: what the
engine must support, what to give the module and what it gives back, how
JavaScript values and functions cross, and how two garbage collectors share
them. The IR itself is in [`../ir.md`](../ir.md); this is what an embedder,
like Bolt's runtimes (`runtime/browser.mjs` and `runtime/node.mjs` in Bolt),
needs to know.

## What the engine needs

Values live in linear memory, so the module needs no Wasm GC. It uses:

| Feature | When | Engines |
|---|---|---|
| Tail calls | always | Wasm 3.0 |
| Reference types (`externref`, `funcref`, tables of them) | host values, host functions | Wasm 2.0 |
| Typed function references (`(ref extern)`) | host values | shipped with Wasm GC |
| Exception handling, the `exnref` form (`try_table`, `throw_ref`) | handlers with an aborting clause, and counting with handlers | Chrome 137, Safari 18.4, Firefox 131 (planned; check) |
| Stack switching | only with `Options::stack_switching` | not in browsers yet; Wasmtime on x86-64 |

**Node 24** (V8 13.6) needs `--experimental-wasm-exnref` for `try_table`;
a module without exceptions runs without it. Later Node versions, from a V8
of Chrome 137 on, shouldn't.

`conduit-wasm`'s tests validate every module with Wasm GC off, so a module
that validates there runs on an engine without it.

## Instantiating a module

```js
const module = await WebAssembly.compile(bytes, { builtins: ["js-string"] });
let exports;
const imports = {
  // Each extern with `#[link(wasm, module = "m", name = "n")]` is `m.n`.
  env: { /* .. */ },
  // Only for a module that uses `host_function`: see "JavaScript calling
  // Conduit".
  conduit: { function: hostFunction },
};
const instance = await WebAssembly.instantiate(module, imports);
exports = instance.exports;
```

What a module imports:

- **Its externs,** each from the module and name of its
  `#[link(wasm, ..)]`. An extern with another link, for another target, is
  an error of `emit_module`.
- **`conduit.function`,** if it uses `host_function`, after its externs.
- **`wasm:js-string`,** if its externs say so, which the engine gives
  itself with `builtins: ["js-string"]` (see "Strings").

What it exports:

- each function with `#[export(wasm, name = "..")]`, under that name;
- `memory`, its linear memory, which glue code reads, like the bytes of a
  string;
- with `Options::counts`: `conduit_live`, a global with the number of
  objects allocated and not freed, and with `Options::cycles` too,
  `conduit_collect()`, which collects the cycles buffered so far;
- with `host_function`: `conduit_release(closure)`;
- with handlers or a shadow stack: `conduit_reset()`, which the host
  calls after an error (see below).

An export starts with no handlers: an effect operation it doesn't handle
itself traps (see "Exports start with no handlers" in `ir.md`).

A JavaScript exception that unwinds through Conduit code, from an extern
or a callback, leaves the module as it was for whoever called it, so a
host can catch it, even in the middle of a call into the module, and go
on. A trap can't be caught in Wasm, so after one the host calls
`conduit_reset()`, at a point where no call into the module is still
running, like its event loop; it's safe to call after any error there:

```js
try {
  exports.on_click(event);
} catch (e) {
  exports.conduit_reset?.();
  reportError(e);
}
```

What the unwound code held leaks. See
[`internals/wasm-unwinding.md`](../internals/wasm-unwinding.md).

## Host values

A JavaScript value, of any kind, is a `host` in Conduit: an object that
Conduit can't look into, which it holds in variables, fields, closures,
refs and `any`s, and passes to externs. A `host` is never null; a
`nullable host` is a host value or null, with `host_null()`,
`host_is_null(x)` and `host_unwrap(x)`, which traps on a null. Wasm's
types only tell a value from a null, not a string from a DOM node, so
these two are all Conduit has: what kind of value one is, the frontend's
types say, like Bolt's `JsString`, and the host checks.

**Inside the module,** a host value is a box of 16 bytes, counted like any
other object, holding the number of a slot in a table of `externref`s. So
it's an `i32` wherever it's held. A null `nullable host` is the address 0,
which no box has, so it costs nothing and isn't counted.

**At the boundary,** it's the host's own reference:

| | Parameter | Result |
|---|---|---|
| extern (an import) | `externref`, both kinds | `host`: `(ref extern)`; `nullable host`: `externref` |
| export | `host`: `(ref extern)`; `nullable host`: `externref` | `externref`, both kinds |

A JavaScript `null` where the type is `(ref extern)` throws a `TypeError`
at the call: from an extern that's declared to give a `host`, or given to
an export that takes one. `undefined` is a value, not a null, and is
accepted anywhere. These are exactly the JS String Builtins' types, whose
functions take `externref`s and give their strings as `(ref extern)`.

An extern call reads each host argument out of its slot with `table.get`,
and puts each host result in a box: one from a free list of boxes, which
has a slot already, or a new one, with a slot that `table.grow` adds. An
export does the reverse under a wrapper.

The flat ABI (`#[abi(flat)]`), for WASI and other C-like hosts, can't pass
host values.

## Strings

Conduit has no string type: a frontend's strings are what it makes them,
like Bolt's UTF-8 `String`, an `imm [i8]`. A JavaScript string is a host
value, and the **JS String Builtins** work on them without leaving Wasm.
Declare them as externs of the module `wasm:js-string`:

```
#[link(wasm, module = "wasm:js-string", name = "length")]
extern length(host) -> (i32)
#[link(wasm, module = "wasm:js-string", name = "substring")]
extern substring(host, i32, i32) -> (host)
```

`length`, `charCodeAt`, `codePointAt`, `concat`, `substring`, `equals`,
`compare`, `fromCharCode`, `fromCodePoint`, `test` and `cast` only take and
give host values and numbers, so they're plain externs. The engine only
runs its builtins with `builtins: ["js-string"]`, and only when an import's
type is exactly the builtin's, which Conduit's types for host values are.
`fromCharCodeArray` and `intoCharCodeArray` take Wasm GC arrays, which
Conduit has none of.

**Without the builtins,** where an engine doesn't have them or they're off,
give the same imports as JavaScript functions, `{ length: (s) => s.length,
.. }`, which run slower.

**Converting** between a string in memory and a JavaScript one is glue:
JavaScript can't read a host value out of memory, and Wasm can't read a
JavaScript string's code units without the GC arrays. Bolt's runtime does
it in two calls, one giving the runtime the text and one taking the
result, with a `TextDecoder` over `exports.memory.buffer` and a
`TextEncoder` back.

**String literals** are built at run time today, from UTF-8 or a code unit
at a time. Imported string constants, which the engine gives with
`importedStringConstants`, are planned: #39.

## JavaScript calling Conduit

**Exports** take and give host values as above, so JavaScript can call a
Conduit function with a DOM node and get one back:
`exports.main(document)`.

**Host functions** are closures that JavaScript calls, like an event
listener: `host_function(c)` takes a closure and gives a `host` that's a
JavaScript function calling it. The closure's parameters and results must
be scalars or host values, and it runs with no handlers, as an export does.

JavaScript functions can only be made in JavaScript, so the module imports
one function from the runtime, and the runtime gives back what calls the
closure:

```js
// When JavaScript lets go of a host function, its closure goes too.
const registry = new FinalizationRegistry((closure) => exports.conduit_release(closure));

/** `conduit.function`: a JavaScript function of trampoline `f` and closure `c`. */
function hostFunction(f, c) {
  const fn = (...args) => f(c, ...args);
  registry.register(fn, c);
  return fn;
}
```

- `f` is a **trampoline**, one for each signature of closure, as a
  `funcref`, which JavaScript calls like any exported function. It takes
  the closure's address first, then the arguments, and runs like an
  export: it empties the handler stack, converts host values both ways,
  and applies the closure, keeping it for the next call.
- `c` is the closure's **address**, a number. The JavaScript function
  holds the closure from then on: `conduit_release(c)` drops it, which the
  registry calls once the function is garbage.
- After lowering effects by evidence passing, every closure takes
  evidence, which JavaScript can't give, so `host_function` gets a closure
  of an adapter that makes fresh evidence and applies `c`.
- A JavaScript function can call a host function at any time, even inside
  an extern call that's still running, as `Array.prototype.map` or
  `dispatchEvent` do: the trampoline saves the handler stack and puts it
  back, and the shadow stack just goes on.

## Garbage collection

Two collectors decide when a host value goes: Conduit's counts, and
JavaScript's own collector. Each sees only its own heap.

- **Conduit keeps what it holds alive.** While anything in Conduit refers
  to a host value, JavaScript can't collect it, whatever else lets go of
  it: the table of `externref`s is a root for JavaScript's collector. Once
  Conduit's last reference is dropped, the box clears its slot, and
  JavaScript collects the value when nothing of its own refers to it
  either. So a host value lives as long as the longer of the two holds it.
  The table never shrinks, but slots are reused, so it's as big as the
  most host values alive at once.
- **Without counts, nothing goes.** A module lowered without
  `Options::counts` never drops, so every host value it got stays alive,
  and JavaScript's objects with it, as long as the instance does.
- **JavaScript keeps a host function's closure alive,** until the
  `FinalizationRegistry` calls `conduit_release`, at some point after the
  function is garbage, or never: finalizers aren't guaranteed to run.
- **Conduit's own cycles** of `mut` objects, refs and `any`s are collected
  by its cycle collector, with `Options::cycles`, when its buffer fills or
  when `conduit_collect()` asks.
- **A cycle through both heaps leaks.** Conduit's cycle collector follows
  only Conduit objects, and JavaScript's can't see into Conduit's memory,
  so a cycle through both is garbage neither can find. It takes
  JavaScript holding a Conduit value, which a host function does. The
  usual case is an event listener that captures the element it's on: the
  element holds the listener, the listener the closure, and the closure
  the element. Both stay until the page goes, even after the element
  leaves it. Rust's `wasm-bindgen` closures have the same limit.

A frontend avoids such cycles rather than collect them:

- don't capture what holds the callback: a listener gets its element from
  the event (`event.currentTarget`) instead;
- let go explicitly: remove the listener (`removeEventListener`, or an
  `AbortSignal` given to `addEventListener`), which drops JavaScript's
  reference to the closure, and so the closure's to the element;
- hold the value weakly, through a JavaScript `WeakRef` that the closure
  captures in its place.

## Exceptions and traps

A JavaScript exception from an extern call unwinds through Conduit's
frames to whoever called the module, as a trap does. An extern that may
throw must say so, with `does throw`: one that throws without it is
undefined behavior, after which the instance must not be used. Where it
says so, the frames an exception unwinds drop what they own, and an
entry point puts the module's state back, so a host that catches the
exception, even in the middle of a call into the module, can go on using
the instance. Catching JavaScript exceptions in Conduit, by handling
`throw`, and throwing them, by performing it, are planned: see
[`../exceptions.md`](../exceptions.md). Conduit's own exceptions, for
aborting handlers, use a tag of their own, which JavaScript code never
sees.

A trap leaves the module's shadow stack where it was: a host that keeps
using the instance after one calls `conduit_reset` first (see
[`../internals/wasm-unwinding.md`](../internals/wasm-unwinding.md)).

## The evaluator

`conduit-eval` has no JavaScript, but its hosts get the same values, so a
frontend can test code that uses them:

- a host value is `Value::Host(n)` to a host function, the number the host
  chose, and a null `Value::Null`; in the machine, it's an object, counted
  and checked for leaks like others;
- a host function is `Value::Function(k)`, which the host calls with
  `Machine::call_function(k, args)` and lets go of with
  `Machine::release_function(k)`. The machine doesn't run while it calls the
  host, so a host can only call one after the call that gave it returns, as
  from an event loop, not during it, as `Array.prototype.map` would;
- `Machine::call` takes and gives host values as `Value::Host(n)`, as an
  export does.

The test programs' hosts, in `conduit-test`, have `wrap(i32) -> (host)`,
`unwrap(host) -> (i32)`, `maybe(i32) -> (nullable host)`, which is null for
a negative number, and `unwrap_or(nullable host, i32) -> (i32)`, in the
evaluator and in Wasmtime alike.

## Open

- #35: keep host values that are never stored in Wasm locals, without a
  slot, for code that makes many short-lived strings.
- #38, #63: catch JavaScript exceptions, by handling `throw`, and throw
  them, by performing it (see [`../exceptions.md`](../exceptions.md)).
- #39: imported string constants for string literals.
- Externs that suspend, for promises, through JSPI: an open question of
  the effect system.
