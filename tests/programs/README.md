# Test programs

Each `.conduit` file here is a module in the text format (see "Text
format" in `docs/ir.md`), with checks in comments that start with `//#`.
Programs name their functions, types, effects, handlers, variables and
labels, except those in `invalid`, whose checks match the verifier's
messages, which use numbers.
Every part of Conduit runs the checks that apply to it:

| Crate | What it does with each program |
|---|---|
| `conduit-ir` | Parses it, checks that printing and parsing gives the same module, and verifies it |
| `conduit-eval` | Runs its `run` checks |
| `conduit-wasm` | Lowers it, validates the Wasm without the GC and stack switching proposals, compares `wat` snapshots and runs its `run` checks with Wasmtime. A program with a handler whose clauses aren't all tail-resumptive or aborting is lowered by evidence passing, and again with stack switching, which is then allowed |
| `conduit-opt` | Optimizes it, verifies the result, compares `opt` snapshots and runs its `run` checks on the result with the evaluator. With `opt: passes`, it does this both after all passes and after only those |
| `conduit-opt` (`effects`) | Lowers its effects by evidence passing, checks that the result verifies and has no effects left, and runs its `run` checks with the evaluator, then again after optimizing |
| `conduit-opt` (`counts`) | Inserts reuse and reference counts, as written, after optimizing, and after evidence passing, and runs its `run` checks with the evaluator counting and collecting cycles: every count must be right. Compares `counts` snapshots |
| `conduit-opt` (`bench`) | Compares `bench` snapshots |


So a `run` check is checked by the evaluator, by Wasm and after
optimization, and they all have to agree.

`cargo test` shows one test per file in each crate, named after its path,
like `control/factorial`. `cargo test factorial` runs only the files whose
path contains `factorial`. A crate lists a file as ignored when none of its
checks apply to that crate.

## Checks

| Check | Meaning |
|---|---|
| `//# run fac(5i32) == 120i32` | Calling `fac` gives `120i32`. Several results are `== (1i32, 2i32)`, none are `== ()`. The function is written as the text format writes it: a name like `fac`, a number like `fn1`, or both, `fac#1`. |
| `//# run fn1(0i32) traps` | Calling `fn1` stops with a trap: a signed division that overflows, an index out of bounds, `trap`, or a continuation resumed twice. Reaching `unreachable` is undefined behavior, which the evaluator reports as an error rather than a trap. |
| `//# verify-error: message` | The verifier rejects the program with an error containing `message`. Only `conduit-ir` looks at such programs. |
| `//# wasm-error: message` | The Wasm backend can't lower the program, with an error containing `message`. |
| `//# wasm-skip: reason` | The Wasm backend doesn't look at the program, for example because it uses a feature the backend doesn't have yet. |
| `//# wat` | The program's Wasm, as text, matches `<name>.wat` next to it. |
| `//# opt` | The program after optimization matches `<name>.opt.conduit` next to it. |
| `//# opt: dce, copies` | The same, but after only those passes, in that order, each run until nothing changes. |
| `//# counted` | The program counts its references by hand: the evaluator runs its `run` checks counting, and each run must free everything but its results. |
| `//# counts` | The program with reuse and counts inserted and drops specialized matches `<name>.counts.conduit`. |
| `//# bench` | What each `run` check does in the evaluator matches `<name>.bench`: steps, allocations, frees, the most objects live at once, count ops, reuse and cycle collections, after the optimizer (`optimized`), and with reuse and counts too, cycles collected (`counted`). These are the same every time, so a pass that makes a program do more shows up as a diff. |

Arguments and results are literals as the text format writes them: `5i32`,
`-1i64`, `1.5f32`, `1e300f64`, `f32_bits(2143289344)`. Floats are compared
by their bits, so `-0.0f64` and `0.0f64` differ. A function under test can
only take and give scalars.

A check that doesn't parse, or an unknown check, fails the file in every
crate.

## Snapshots

`wat`, `opt`, `counts` and `bench` compare against a file next to the program. When one is
missing or differs, the test fails with the lines that differ. To create or
update them, run the tests with `BLESS=1`:

```sh
BLESS=1 cargo test --workspace --test programs
```

and review the changes with `git diff` before committing them.

A program can have several `opt` checks, for example `//# opt: dce` and
`//# opt: dce, copies`, to show what a pass does on its own and together
with others. Its `<name>.opt.conduit` then has a section for each check, in
the order they appear, headed `// after dce` or `// after all passes`. With
a single `opt` check there is no header.

## Host functions

Programs can import these host functions, which every runner provides:

| Module | Name | Signature | Gives |
|---|---|---|---|
| `"test"` | `"double"` | `(i32) -> (i32)` | twice its argument |

For example: `extern extern0 "test" "double" (i32) -> (i32)`.
