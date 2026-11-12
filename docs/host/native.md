# Native hosts

How to run a module natively: build it with the LLVM backend
(`conduit-llvm`, see [llvm.md](../backends/llvm.md)), link it with C code that
provides its externs and calls its exports, and what that C code can rely
on. The backend is experimental; this is what it does today, tested on
Linux on x86-64.

## Building

The pipeline is the one for Wasm (see [frontends.md](../frontends.md)),
with the LLVM backend at the end. It gives LLVM IR as text, which `clang`
compiles; no LLVM library is linked.

```rust
let mut m: conduit_ir::Module = /* built by the frontend */;
conduit_ir::verify::verify_module(&m)?;
conduit_opt::optimize_module(&mut m);
// Handlers that capture their continuations become ordinary code first.
if !conduit_ir::clause::direct_handlers(&m) {
    conduit_effects::lower_effects(&mut m)?;
}
// Last, so that memory is freed.
conduit_opt::ownership::insert_counts(&mut m)?;
let options = conduit_llvm::Options { counts: true, cycles: true };
std::fs::write("module.ll", conduit_llvm::emit_module_with(&m, &options)?)?;
```

```sh
clang -O2 -Wno-override-module module.ll host.c -o program
```

- **`-Wno-override-module`:** the IR names no target, so `clang` uses its
  own, and says so otherwise.
- **`counts`** frees memory by reference counting, for a module that
  `insert_counts` ran on; without it, nothing is ever freed. **`cycles`**
  also collects what counting alone can't, as in Wasm.
- **Linking** needs only what a C program has: `malloc`, `calloc`,
  `realloc` and `free`, and for handlers that abort and for exceptions,
  the unwinder (`libgcc_s` or `libunwind`, with
  `__gcc_personality_v0` and `_Unwind_ForcedUnwind`), which C programs
  link on Linux and macOS.

## Symbols

Every extern is a C function the host defines, under the symbol its
`#[link(c, symbol = "..")]` gives, and every export is one the module
defines, under its `#[export(c, symbol = "..")]`'s. Externs that link to
the same symbol share it, so they must agree on its signature.

Besides those, the module defines:

| Symbol | With | What |
|---|---|---|
| `int64_t conduit_live` | counts | how many objects are allocated and not freed |
| `void conduit_collect(void)` | counts | collects the cycles buffered so far (nothing without `cycles`) |
| `void conduit_release(void *closure)` | `host_function` | gives back a closure the host held (below) |

and expects, when it uses `host_function`, the host to define
`conduit_host_function` (below). Its other symbols are its own: all of
them are `fn` and a number, like `fn3` or `fn3_entry1`, or start with
`conduit_`, so the host mustn't define symbols of those names.

## Calling convention

Externs and exports use C's calling convention, with these types:

| Conduit | C |
|---|---|
| `i32`, `i64` | `int32_t`, `int64_t` (signless in Conduit: the ops say how to read them) |
| `i8`, `i16` | passed as LLVM's `i8` and `i16`, not extended: see below |
| `f32`, `f64` | `float`, `double` |
| `host`, `nullable host` | a `conduit_host`, or only its `void *value` as an extern's parameter (see "Host values") |
| any other object | a pointer to Conduit's object, which the host can only pass back |

**Narrow integers** aren't extended at this boundary: C compilers pass
an `int8_t` or `int16_t` extended to 32 bits, and expect it so, which the
module doesn't do. Use `i32` in externs' and exports' signatures, or the
flat ABI, which zero-extends them to `int32_t`.

Of several results, the first is returned, and each other one is written
through a pointer the caller passes after the arguments:

```
#[export(c, symbol = "divmod")]
fn divmod(%a: i32, %b: i32) -> (i32, i32) { .. }
```

```c
int32_t divmod(int32_t a, int32_t b, int32_t *rem);
```

An `#[abi(flat)]` extern takes arrays as C does (see "What it handles" in
[llvm.md](../backends/llvm.md)): an array as a pointer to its elements and its
length as an `int32_t`, or only the pointer for `#[addr]`; a list of byte
buffers as a `struct iovec *` and how many there are; an `i8` or `i16`
zero-extended to an `int32_t`. The host may write only `mut` arrays, and
only during the call.

## Host values

A host value is the host's own pointer, with the functions that keep it
alive and let it go, so that one module can hold values of different
libraries:

```c
typedef struct conduit_host_ops {
  void (*retain)(void *value);
  void (*release)(void *value);
} conduit_host_ops;

typedef struct conduit_host {
  void *value;                 // null for a null `nullable host`
  const conduit_host_ops *ops;
} conduit_host;
```

The host declares these itself, with these layouts: no header comes with
the module. Inside it, a host value is a counted box holding one of the
host's references, and the host only hears of it when it comes in and
when it goes:

| Where | As | Who owns it |
|---|---|---|
| extern parameter | `void *value` | borrowed for the call: a host that keeps it calls `retain` itself |
| extern result | `conduit_host` | the host gives Conduit a reference; a null where the type is `host` traps |
| export parameter | `conduit_host` | the caller gives Conduit a reference |
| export result | `conduit_host` | Conduit calls `retain` for the caller |

A host value among the results after the first is written through its
pointer as a `conduit_host`, owned as one returned is. The flat ABI
can't pass host values. When the box's count gets to 0, Conduit calls
`release`. `retain` and
`release` run from inside Conduit's code, drops among it, so they must
not throw; and since they run in the middle of it, they're best kept from
calling into the module. Without counts, nothing is released.

### Functions the host calls

`host_function(c)` gives the host a function that runs the closure `c`.
A C function pointer can't hold a closure, so the host makes the
function from two pointers, by defining:

```c
conduit_host conduit_host_function(void *trampoline, void *closure);
```

`trampoline` is a C function for the closure's signature,
`ret trampoline(void *closure, args.., outs..)`, which runs it as an
export runs its function. The host wraps the two in a value of its own,
which it gives back as a `conduit_host`, whose reference Conduit owns.
The host keeps the closure for as long as it holds the function, and
gives it back with `conduit_release(closure)`. A cycle through Conduit
and the host, like a callback that captures what holds it, leaks.

## Running

- **One thread at a time.** The heap's free lists and the handler stack
  are globals, so nothing in the module may run on two threads at once.
- **Exports start with no handlers.** An operation that nothing in the
  export handles traps, even when the host calls the export from inside
  an extern that runs under a handler.
- **A trap ends the process,** with `llvm.trap`: unlike Wasm, there's no
  catching one and going on.
- **Memory** comes from `malloc`, in chunks that are never given back:
  freed objects go on free lists the module reuses. `conduit_live` says
  how many are live; a host checking for leaks calls `conduit_collect`
  first, if cycles are collected.

## Exceptions

A C host has none, so this is for C++ hosts (see
[exceptions.md](../ir/exceptions.md)):

- **An extern that may throw** must have `throw` in its row,
  `does throw`. One without that throws is undefined behavior, and is
  declared to LLVM as not unwinding. With it, the frames an exception
  unwinds drop what they own, with C's personality, which runs cleanups
  for any exception, and an export or trampoline it unwinds gives the
  host back its handler stack, so a host that catches the exception can
  go on calling into the module.
- **Catching** what a C++ host throws in Conduit isn't implemented yet: a
  handler for `throw` only catches what Conduit code raises, and a
  `raise` that nothing handles traps (#66).

## An example

A module that greets someone by a name the host gives it, and divides:

```
#[link(c, symbol = "make_name")]
extern make_name(i32) -> (host)

#[link(c, symbol = "greet")]
extern greet(host, i32) -> ()

#[export(c, symbol = "greet_twice")]
fn greet_twice(%which: i32) -> (i32) {
  let %n: host = call_extern make_name(%which)
  let = call_extern greet(%n, 2i32)
  jump ret(2i32)
}

#[export(c, symbol = "divmod")]
fn divmod(%a: i32, %b: i32) -> (i32, i32) {
  let %q: i32 = div.s(%a, %b)
  let %r: i32 = rem.s(%a, %b)
  jump ret(%q, %r)
}
```

and its host, whose names are counted strings, which Conduit lets go of
once it's done with them:

```c
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct conduit_host_ops {
  void (*retain)(void *value);
  void (*release)(void *value);
} conduit_host_ops;

typedef struct conduit_host {
  void *value;
  const conduit_host_ops *ops;
} conduit_host;

typedef struct { int refs; const char *text; } name;
static void name_retain(void *v) { ((name *)v)->refs++; }
static void name_release(void *v) {
  name *n = v;
  if (--n->refs == 0) free(n);
}
static const conduit_host_ops name_ops = {name_retain, name_release};

// The externs.
conduit_host make_name(int32_t which) {
  name *n = malloc(sizeof *n);
  n->refs = 1;
  n->text = which ? "world" : "Conduit";
  return (conduit_host){n, &name_ops};
}
void greet(void *value, int32_t times) {
  for (int32_t i = 0; i < times; i++) printf("hello, %s\n", ((name *)value)->text);
}

// The module's.
extern int64_t conduit_live;
void conduit_collect(void);
int32_t greet_twice(int32_t which);
int32_t divmod(int32_t a, int32_t b, int32_t *rem);

int main(void) {
  if (greet_twice(1) != 2) return 1;
  int32_t rem;
  if (divmod(17, 5, &rem) != 3 || rem != 2) return 1;
  conduit_collect();
  return conduit_live != 0;
}
```

Built as above, it prints `hello, world` twice, and exits with 0: the
name was released, and nothing is left live.

The tests have more: `tests/programs/host/c.c` is the host the test
programs run with, and `crates/conduit-llvm/tests/host_exports.rs`
has hosts in C that take and give host values, make functions with
`host_function`, call exports from inside externs, and, in C++, throw.
