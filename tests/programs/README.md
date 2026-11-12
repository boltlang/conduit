# Test programs

Each `.conduit` file here is a module in the text format (see
`docs/ir/text.md`), with checks in comments that start with `//#`.
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
| `conduit-effects` | Lowers its effects by evidence passing, checks that the result verifies and has no effects left, and runs its `run` checks with the evaluator, then again after optimizing |
| `conduit-opt` (`counts`) | Inserts reuse and reference counts, as written, after optimizing, and after evidence passing, and runs its `run` checks with the evaluator counting and collecting cycles: every count must be right. Compares `counts` snapshots |
| `conduit-bench` | Compares `bench` snapshots |


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
| `//# bench-run f(..) == ..` | A call as `run` writes one, which only `conduit-bench` makes, when it times: one too big for the other tests, which make every `run` each time. The versions by hand (below) make it too. |
| `//# verify-error: message` | The verifier rejects the program with an error containing `message`. Only `conduit-ir` looks at such programs. |
| `//# wasm-error: message` | The Wasm backend can't lower the program, with an error containing `message`. |
| `//# wasm-skip: reason` | The Wasm backend doesn't look at the program, for example because it uses a feature the backend doesn't have yet. |
| `//# wat` | The program's Wasm, as text, matches `<name>.wat` next to it. |
| `//# opt` | The program after optimization matches `<name>.opt.conduit` next to it. |
| `//# opt: dce, copies` | The same, but after only those passes, in that order, each run until nothing changes. |
| `//# counted` | The program counts its references by hand: the evaluator runs its `run` checks counting, and each run must free everything but its results. |
| `//# counts` | The program with reuse and counts inserted and drops specialized matches `<name>.counts.conduit`. |
| `//# host` | The program calls the test host, which each target declares in its own file: see "Host functions". |
| `//# max-frames: 100` | The evaluator runs each `run` check with at most that many frames, and stops with a stack overflow past them, instead of its default of 100000. A program that must run in constant stack, like a loop written as tail calls, checks it with a small limit. |
| `//# bench` | What each `run` check does in the evaluator matches `<name>.bench`: steps, allocations, frees, the most objects live at once, count ops, reuse and cycle collections. A row for each way of compiling it, each adding one thing after the optimizer: no counts (`optimized`), `counts` with every parameter owned, `+borrows`, `+reuse`, `+specialized` drops, and `+cycles` collected. These are the same every time, so a pass that makes a program do more shows up as a diff. `bench/` has benchmarks from the Perceus paper's set. |

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

## Benchmarks

The programs with a `bench` check are benchmarks too: `bench/` has some in
the style of the Perceus paper's. Their `.bench` snapshots show what the
evaluator does, which is the same every time. `conduit-bench` runs them as
Wasm in Wasmtime, compiled each of the same ways, and measures fuel
(about one per Wasm instruction run, also the same every time), time,
code size and memory:

```sh
cargo run --release -p conduit-bench -- --json before.json
# change something
cargo run --release -p conduit-bench -- --compare before.json
```

`--fuel` skips the timing, `--config optimized,+reuse` picks the ways of
compiling, and a word picks the programs, like `nqueens`. The benchmarks
of data structures also have versions written by hand, `<name>.c` and
`<name>.rs`, the same work with memory managed by hand, which
`--backend native,c,rust` times next to Conduit's native code as the
floor to compare with. CI does this for
each pull request (`.github/workflows/bench.yml`): it compares the fuel
with the base's, shows what changed in the job's summary
(`--markdown`), and fails if any went up by more than 2% (`--check 2`),
unless the pull request has the label `bench-ok`. For each push to
`main`, it also writes the README's badges (`--badges`) to the branch
`badges`. Times are only
worth comparing between runs on the same quiet machine, and within about
5%: the ways of compiling take turns, a call each, so the machine slowing
down slows them all, but two modules with the same code can still differ
that much.

`--backend` runs them on other backends too, side by side, with how each
time compares with Wasm's:

```sh
cargo run --release -p conduit-bench -- --backend wasm,wasm-switch,native
```

- `wasm`, the default, and the only one CI runs: Wasm, with handlers that
  need it lowered by evidence passing.
- `wasm-switch`: Wasm with stack switching for those handlers, for a
  program that has some. Wasmtime runs it only on x86-64 Linux and macOS.
- `native`: the LLVM backend, compiled with `clang`, with handlers that
  need it lowered by evidence passing, as for `wasm`. It has no fuel. Each timed call runs in a
  forked process, after a call that warms it up, pinned to one core on
  Linux (see `conduit_llvm::native::TIMER`). Without `clang`, it's
  skipped.

`effects_state`, `effects_generator` and `effects_exceptions` measure
handlers: tail-resumptive clauses, clauses that keep their continuation,
and aborting ones, each kept from being optimized away.
`effects_rare_exceptions` aborts once every 1024 rounds instead of every
8th, for what a `handle` costs when nothing aborts to it.

## Host functions

A program with a `//# host` check can call the test host's functions,
which every runner provides. The program doesn't declare them: each
target declares them in its own file under `host/`, with its own links,
which goes after the program's text when it's built for that target
(`conduit_test::Target`), as a frontend picks the links for the target it
compiles for:

| File | Target | Host |
|---|---|---|
| `host/emu.conduit` | the evaluator | `conduit_test::eval::TestHost` |
| `host/wasm.conduit` | the Wasm runners | the module `"test"` of their linker |
| `host/c.conduit` | native, through LLVM | `tests/programs/host/c.c`, as `conduit_test_<name>` |

They declare the same functions with the same signatures, which a test
in `conduit-test` checks.

| Extern | Signature | Gives |
|---|---|---|
| `double` | `(i32) -> (i32)` | twice its argument |
| `double_io` | `(i32) -> (i32) does io` | the same, requiring the effect `io` |
| `double_throw` | `(i32) -> (i32) does throw` | the same, as an extern that may throw an exception, though it never does |
| `sum` | `#[abi(flat)]`, `(imm [i8]) -> (i32) does io` | the sum of its bytes, as signed |
| `fill` | `#[abi(flat)]`, `(mut [i8], i32) -> () does io` | sets every byte to the second argument's low byte |
| `gather` | `#[abi(flat)]`, `(imm [imm [i8]]) -> (i32, i32) does io` | how many buffers, and the sum of all their bytes |
| `squares` | `#[abi(flat)]`, `(#[addr] mut [i32], i32) -> () does io` | sets elements 0 to n - 1 to their index squared |
| `scale` | `#[abi(flat)]`, `(i32) -> (i32, i64) does io` | 0, and its argument times 10^12 |
| `byte` | `#[abi(flat)]`, `(i8) -> (i32) does io` | its argument zero-extended |
| `wrap` | `(i32) -> (host)` | a host value holding its argument |
| `unwrap` | `(host) -> (i32)` | what a host value holds |
| `maybe` | `(i32) -> (nullable host)` | null for a negative argument, else a host value holding it |
| `unwrap_or` | `(nullable host, i32) -> (i32)` | what a host value holds, or the second argument for null |

The host files also declare `effect io {}`, which the flat ones require,
so a program that uses them doesn't declare `io` itself.
