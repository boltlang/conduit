# Exceptions

How Conduit deals with exceptions that aren't its own: one a host throws
from an extern, like any JavaScript function can, and one Conduit code
throws to its host. The rules here are what frontends, backends and hosts
can rely on. Each section says whether it's implemented yet.

Conduit's own aborts, an aborting handler's clause leaving its body, are
effects (see [effects.md](effects.md)), and traps stop the
program: neither is an exception in this sense, though backends may use
their exception machinery for aborts.

## `throw` is an effect

`throw` is a built-in effect with one operation:

```
effect throw {
  (nullable host) -> ()    // raise: the value thrown
}
```

No module declares it, and no other effect can be named that, except to
say where the host's exceptions come from, for the target the module is
for, as an extern's link says where its function is:

```
#[link(wasm, module = "bolt", name = "js_tag")]
effect throw
```

In Wasm, that's the tag the module imports, `WebAssembly.JSTag` in
JavaScript, under the name the frontend chose: no runtime of Conduit's
provides it. Without a link, a handler for `throw` catches only what
Conduit code raises, and the host's exceptions unwind as they would
through any code (rule 4 below). It's
written like any effect, first in a row: `does throw + Log`, and its
operation is `throw.0`, called `raise` here. `Any` has it.

**Where it is in a row** is where an exception can come from:

- An extern that may throw has `throw` in its row:

  ```
  #[link(wasm, module = "dom", name = "query")]
  extern query(imm [i8]) -> (host) does throw
  ```

  Calling it is like performing `raise` with what it throws, at the
  call.
- Code that can reach such a call, or perform `raise` itself, has it too,
  as with any effect: the verifier checks it.
- **An extern that can call back into an export which can throw** can
  throw too, so it needs `throw` in its row. Making sure it has is the
  frontend's job.

**Throwing** from Conduit code is `perform throw.0(%e)`, for a host
value `%e`, which may be null, as JavaScript can throw `null`.

**Catching** is handling `throw`: a handler for it takes `throw` out of
its body's row, as a handler of any effect does, and its clause gets
what was thrown.

```
handler catch for throw env() ops(caught) ret fine

fn caught(%e: nullable host, %k: cont() -> (R)) -> (R) { .. }  // never resumes
fn fine(%r: ..) -> (R) { .. }
```

*Status:* implemented: `throw` is an effect under the reserved
`EffectId::THROW`, whose declaration a module gets with
`Module::declare_throw` (the text format adds it), and Conduit code can
perform and handle it in every implementation. Catching what the host
throws isn't implemented yet: see below.

## Rules

1. **A clause for `raise` never resumes.** By the time it runs, the host
   frames that threw are gone, so there's nothing to resume, and an
   extern that threw gave no results. The verifier requires every
   clause for `throw.0` to be aborting: never to use its continuation
   (see `conduit_ir::clause`). That's what stands in for a type of no
   values, which Conduit doesn't have.
2. **A `raise` that nothing handles goes to the host.** Any other
   operation that nothing handles traps; this one leaves the entry point
   the host called as the host's own exception, with the value thrown,
   as an uncaught exception would. So an export may throw to its caller,
   and an exception round-trips through callbacks: an export that raises
   throws to the host, and the extern call that called the host raises
   it again.
3. **An extern without `throw` that throws is undefined behavior.** Once
   an exception unwinds Conduit code whose row doesn't have `throw`, the
   instance's state is unspecified, and the host must not call into it
   again. That's what lets code that can't throw do without cleanups, a
   backend declare such calls as not unwinding, and the optimizer move
   writes across them. A sanitizer may check it (#61).
4. **What an exception unwinds is dropped.** With counts, each frame an
   exception leaves drops what it owns, as for an abort: the calls where
   one can leave a frame are those that can reach a `raise`, the
   `call_extern`s with `throw` among them
   (`conduit_ir::owned::may_unwind`). Code that can't reach one pays
   nothing.
5. **Host value ops never throw.** Natively, `retain` and `release` of a
   `conduit_host_ops` run from drops, which can't have `throw`.

*Status:* 1, 3, 4 and 5 are implemented, and 2 in the evaluator: a
`raise` that nothing handles ends its run with `ErrorKind::Uncaught`, and
in Wasm, with a link, is thrown to the host. Natively it still traps, as
any operation does (#66).

## How the implementations do it

How a handler for `throw` is lowered depends on how the module's other
handlers are, as for any effect, and an exception from the host has to
become a `raise` somewhere:

- **Directly, or with stack switching:** the exception unwinds Conduit
  frames as the host's, running their cleanups, and the `handle` for
  `throw` catches it: in Wasm, its body runs in a `try_table` that
  catches the tag `throw`'s link names. A `try_table` on a suspended
  stack still applies after a `resume`, so stack switching changes
  nothing for a `handle` in a switched body. A `raise` that Conduit code
  performs aborts to the handler as any aborting clause does.
- **Evidence passing:** a handler's frame doesn't outlive a yield, so a
  host exception must not unwind Conduit frames at all. Each
  `call_extern` with `throw` catches what the host throws and performs
  `raise` with it, which then yields like any operation. After lowering,
  no exception from the host unwinds a frame, and `throw` needn't be in
  any row (#62).
- **Entry points** (exports and trampolines of `host_function`): a
  `raise` that reaches one is thrown to the host, as JavaScript's
  exception in Wasm. In a module where nothing has `throw`, entry points
  catch nothing (see
  [`../internals/wasm-unwinding.md`](../internals/wasm-unwinding.md)).
- **Native code** runs cleanups for any exception, with C's personality,
  `__gcc_personality_v0`, which can't catch. Catching a C++ exception
  needs the C++ runtime's, and a hook that makes a `conduit_host` of it,
  so the native backend doesn't handle `throw` yet.
- **The evaluator:** a host function can throw a host value, which the
  call of the extern raises.

*Status:* the cleanups are implemented in Wasm and natively, and so are
the entry points for a `throw` that only unwinds (#59, #40). A `raise`
from Conduit code is caught by a handler for `throw` everywhere, as an
aborting clause's operation is, and the evaluator raises what a host
function throws. In Wasm, with a link, a handler for `throw` catches what
the host throws, and a `raise` nothing handles goes to the host (#64),
except in a handler that also switches stacks, and in modules lowered by
evidence passing, which gives every row of a module that throws `throw`
instead of converting (#65, which would make #62 moot). Natively, what
the host throws isn't caught yet (#66).
