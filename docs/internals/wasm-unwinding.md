# Unwinding through Conduit's Wasm

How the Wasm backend keeps a module's state right when something other
than a Conduit `return` leaves Conduit code: an exception it doesn't
throw itself, like a JavaScript one from an extern, or a trap. It's what
lets a host that catches one go on using the instance, as a page does for
its next event once its callbacks call into Conduit (#40).

The code is in `crates/conduit-wasm/src`: `host_entry` and `lower_reset`
in `lib.rs`, `handle` in `func.rs`, and `save` and `restore` in
`shadow.rs`. The tests are in `crates/conduit-wasm/tests/unwinding.rs`.

## What a module's state is

Besides its objects, a module keeps a few globals that Conduit code
changes on the way in and puts back on the way out:

| Global | What it is | Who changes it |
|---|---|---|
| `HANDLERS` | the innermost handler frame, or 0 | each `handle`, and each entry point, which empties it |
| `SITE` | the handler stack of the `perform` whose clause runs | each clause call, for `discontinue` |
| the shadow stack's pointer and limit | the top of the shadow stack, and the lowest address its current segment has | each function with objects on it, and each `#[abi(flat)]` extern call with buffers on it |

Each saves what it found in a local and puts it back before it returns or
makes a tail call. An exception that unwinds the function skips that, and
so does a trap.

## Who can see the state go wrong

Wasm code of the module can catch only its own exceptions: a handler's
`try_table` catches the `ABORT` tag, or, with counts, catches everything
to give back its frame and rethrows at once. So once an exception that
isn't Conduit's is thrown, no Conduit code runs again until it leaves the
module, through one of the host's entry points:

- an export's wrapper, which the host called;
- a trampoline of `host_function`, which the host called to run a closure.

That's where the host can catch it, and the only place it can go on from.
If it catches it in the middle of a Conduit call, like a JavaScript extern
that calls an export in a `try`, it returns to Conduit code that called the
extern, which expects the globals as they were when it called.

So the rule is: **every entry point gives the host back the state it was
called with, however it's left.** Nothing else has to: the code between
the throw and the entry point never runs, so what it leaves behind is never
seen.

## Entry points

`host_entry` makes every export wrapper and trampoline. When some function
of the module has `throw` in its row, so that an exception from the host
can unwind it, and the module has handlers or a shadow stack, it:

1. saves `HANDLERS`, `SITE`, and the shadow stack's pointer and limit in
   locals, and empties `HANDLERS`, since an export starts with no
   handlers;
2. makes the call inside a `try_table` that catches everything,
   `catch_all_ref`, keeping the results in locals;
3. on an exception, puts the four back and rethrows it, with `throw_ref`,
   so the host sees the exception it would have;
4. on a return, puts `HANDLERS` and `SITE` back, and gives the results.

The shadow stack is only put back on an exception: a function that
returns has popped what it pushed already. Putting it back, with
`shadow::restore`, also gives back the segments chained since the entry
point saved it, which the frames that unwound took and never released.

An export of a module with a shadow stack but no handlers and no host
values needs a wrapper only for that, so it has one only when something
throws.

In a module where nothing has `throw`, an entry point catches nothing: it
empties `HANDLERS` and puts it back when the call returns, as before #40.
An exception that unwinds it anyway came from an extern without `throw`,
which is undefined behavior (see "Exceptions from the host" in
[`../ir.md`](../ir.md)), after which the host must not use the instance
again, so nothing would see the state it leaves.

With stack switching, a continuation runs on shadow stack segments of its
own. Its `drive` function catches what its body throws and switches back
to the outer stack before passing it on, so by the time an exception gets
to an entry point, the shadow stack is the one the entry point saved.

## Handlers

Since entry points put the globals back, a handler only has to give back
its frame, and only when counting, since without counts nothing is freed.
With counts:

- a handler that doesn't abort catches everything around its body, gives
  back its frame, and rethrows;
- an aborting handler catches its own `ABORT` tag, as before, and now
  everything else too, in a second catch, after which it gives back its
  frame and rethrows.

Neither sets the globals back on the way out. The issue proposed they do;
it would be redundant, since nothing reads them before an entry point
does.

## Traps

No Wasm code can catch a trap, and the frames it unwinds run nothing, entry
points included. A trap leaves the globals wherever it found them. The
next call into the module puts its own entry state in place and back, so
`HANDLERS` and `SITE` don't matter after a trap. The shadow stack does: the
pointer stays where the trapping code left it, so each trap leaks what was
on the stack then, and enough of them exhaust its first segment and chain
new ones from the heap.

The issue proposed that the outermost entry point reset the state, found
by a depth counter. That can't work for a trap: the counter is never
decremented on the way out, so after one, the next outermost call can't
tell it's the outermost. Only the host knows when nothing of the module's
is running. So a module with handlers or a shadow stack exports:

```
conduit_reset()
```

which sets `HANDLERS` and `SITE` to 0, and the shadow stack's pointer and
limit to where they start, giving back the segments chained on the main
stack. The host calls it after it catches an error from the module at a
point where no call into the module is still running, like its event
loop. It's safe whenever no call into the module is running, so a host can
call it after any error, exception or trap, without telling them apart.

With stack switching, the trap may have left the shadow stack on a
continuation's segments, which don't chain back to the main stack, so
those aren't given back: they leak, and the pointer and limit are still
reset.

## What still leaks

- **Nothing, for an exception from an extern with `throw` (#59).** With
  counts, each call that can be unwound while its frame owns something is
  in a `try_table` that catches everything, drops what the frame owns
  then (see `conduit_ir::owned`), and rethrows, and a `handle` that's
  unwound drops its handler's environment. The calls that can be unwound
  are those that may perform, for aborts, and the `call_extern`s with
  `throw` and what can reach them, for exceptions from the host. An
  extern without `throw` that throws is undefined behavior: what the
  frames it unwinds own leaks then, and the instance must not be used
  again.
- **After a trap:** the same, and the boxes an entry point made for host
  values it was given, and with stack switching, the shadow stack segments
  of the continuation that trapped. A trap in the middle of a cycle
  collection may leave objects colored, and so never collected.

## Tests

`crates/conduit-wasm/tests/unwinding.rs` runs a module whose extern throws
an exception of another module, as JavaScript would:

- an export calls the host, which calls another export that pushes on the
  shadow stack and throws, and catches it; the first export then performs
  an operation, which must still find its handler, 20,000 times without
  memory growing, with counts;
- an exception through an aborting handler, with counts, gives its frame
  back: `conduit_live` stays the same;
- a function that pushes on the shadow stack and traps, 20,000 times with
  `conduit_reset` after each, leaves memory as it was, and without it,
  memory grows.

Each fails without the fix.
