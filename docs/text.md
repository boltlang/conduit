# Text format

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

**Exports.** `#[export(..)]` before a function, at most one, says how
it's called from outside the module (`exports` of `Module`), for the one
target the module is for, like an extern's `#[link(..)]`:
`#[export(wasm, name = "..")]` as a Wasm export, `#[export(emu, name =
"..")]` by the evaluator's host, or `#[export(c, symbol = "..")]` as a C
function, which nothing lowers yet. The name or symbol is the function's
own by default, and `print` leaves it out when it is. Two exports of one
kind can't share a name. An exported function owns its parameters (see
"Reference counts").

```
#[export(wasm, name = "_start")]
fn start() -> () {
  jump ret()
}
```

**Externs.** An extern is `extern`, its name, its parameters, `->`, its
results and its row. Attributes in `#[..]` come before it and before its
parameters:

- `#[link(..)]`, which every extern has exactly one of, is where the
  host provides it (`link` of `Extern`), for the one target the module
  is for: choosing the kind for each target is the frontend's job.
  - `#[link(wasm, module = "..", name = "..")]` is a Wasm import, which
    only the Wasm backend lowers.
  - `#[link(emu, name = "..")]` is a function of the evaluator's host,
    which only the evaluator calls, with IR values, whatever the ABI.
  - `#[link(c, symbol = "..", lib = "..")]` is a C function, from library
    `lib` if there is one, which nothing lowers yet.

  The name or symbol is the extern's own by default, and `print` leaves
  it out when it is. Each consumer rejects the other kinds: the Wasm
  backend when it lowers the module, and the evaluator when the extern
  is called.
- `#[abi(flat)]` is its ABI (`abi` of `Extern`), when it isn't the
  default, `#[abi(conduit)]`.
- `#[nolen]` before a parameter of an `#[abi(flat)]` extern passes the
  array's address alone.
- A parameter can have a name, `%fd: i32`, which only shows what it is
  (`param_names` of `Extern`).

```
data data11 {
  ()
  (i32, inherit data11)
}

#[link(wasm, module = "env", name = "print")]
extern extern1(i32) -> ()

#[link(wasm, module = "wasi_snapshot_preview1")]
#[abi(flat)]
extern fd_write(%fd: i32, %iovs: imm [imm [i8]]) -> (i32, i32)

#[link(wasm, module = "wasi_snapshot_preview1")]
#[abi(flat)]
extern args_get(#[nolen] %argv: mut [i32], #[nolen] %buf: mut [i8]) -> (i32)

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
  `imm [i32]`, `ro fn(i32) -> ()`, `cont() -> ()`, `ref i32`, `imm any`,
  `host`, `nullable host`.
  A closure or continuation type with effects has them after its
  results, `imm fn() -> (i32) does effect20 + effect21`, and so does a
  function, `fn fn1() -> (i32) does effect20 {`, and a handler,
  `handler handler30 for effect20 does effect21 env(..) ..`. A handler of
  several effects lists them, and its clauses for each, apart by `;`:
  `handler handler31 for effect20, effect21 env() ops(fn4, fn5; fn6) ret fn7`. Without
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
  `handle handler30(..)`, `perform effect20.0(..)`, `resume(..)`,
  `discontinue effect20.0(..)`, `dup(..)`, `drop(..)`, `is_unique(..)`,
  `free(..)`, `decref(..)`, `drop_reuse(..)`, `reuse_token(..)`,
  `reuse data10.1(..)`, `host_null()`, `host_is_null(..)`,
  `host_unwrap(..)` and `host_function(..)`.
  A reuse token's type is `reuse data10.1`.
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
