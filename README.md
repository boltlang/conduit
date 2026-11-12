# The Conduit Compiler Backend

[![Vibe-Scale 2.0(V2|U1|T0): Significant AI, spot-checked](https://img.shields.io/badge/Vibe--Scale%202.0(V2%7CU1%7CT0)-Significant%20AI%2C%20spot--checked-2ca02c)](https://github.com/vibesdk/vibe-scale/blob/main/scale/vibe-2.md#v2-u1-t0-score-20--significant-ai-spot-checked)
[![Rust](https://github.com/boltlang/conduit/actions/workflows/rust.yml/badge.svg?branch=main)](https://github.com/boltlang/conduit/actions/workflows/rust.yml)
[![Counting overhead](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fboltlang%2Fconduit%2Fbadges%2Foverhead.json)](https://github.com/boltlang/conduit/tree/main/tests/programs/bench)
[![Reuse](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fboltlang%2Fconduit%2Fbadges%2Freuse.json)](https://github.com/boltlang/conduit/tree/main/tests/programs/bench)
[![Benchmarks](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fboltlang%2Fconduit%2Fbadges%2Fbenchmarks.json)](https://github.com/boltlang/conduit/tree/main/tests/programs/bench)

Conduit is a highly experimental compiler backend that is meant to be a
backbone for other high-level programming languages. It explicitly does not aim
to serve languages that are bare-metal.

Some of its properties:

 - Effects and effect handlers are built into the language.
 - Mutations and cycles are allowed.
 - ADTs are the default way to construct and manipulate structured data.
 - No need for manual memory management. The compiler optimises allocations
   away where possible and only uses a GC as a fallback.
 - Can target WebAssembly: no extra adjustments needed.
 - The IR defines join points to easily share control flow between multiple
   statements.

## Documentation

Start at [docs/README.md](docs/README.md): the IR, the passes, the
backends, and what a host does to run a module.

 ## Projects Using Conduit

 - [The Bolt Programming Language](https://boltlang.net)
 - Your own language here? Open a pull request!
