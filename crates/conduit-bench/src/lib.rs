//! Benchmarking Conduit: the ways of compiling a program that the
//! benchmarks compare, and how a benchmark gets its module. The `bench`
//! snapshots (`tests/bench.rs`) measure them in the evaluator, and the
//! `conduit-bench` runner (`src/main.rs`) as Wasm.

use conduit_ir::Module;
use conduit_opt::ownership::{
    Borrows, insert_counts, insert_counts_with, insert_reuse, specialize_drops,
};

/// A program's module as the ways of compiling start from: its effects
/// lowered by evidence passing, if its handlers need it, then optimized.
pub fn prepare(mut m: Module) -> Result<Module, String> {
    if !conduit_ir::clause::direct_handlers(&m) {
        conduit_effects::lower_effects(&mut m)?;
    }
    conduit_opt::optimize_module(&mut m);
    Ok(m)
}

/// What a benchmark runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Backend {
    /// Wasm, in Wasmtime, with handlers that need it lowered by evidence
    /// passing.
    Wasm,
    /// Wasm, in Wasmtime, with handlers that need it lowered with stack
    /// switching instead. Only for a program that has such handlers.
    WasmSwitch,
    /// Native code, from the LLVM backend, compiled with `clang`, with
    /// handlers that need it lowered by evidence passing, as for Wasm.
    Native,
}

impl Backend {
    /// Every backend, in the order results show them.
    pub const ALL: [Backend; 3] = [Backend::Wasm, Backend::WasmSwitch, Backend::Native];

    pub fn name(self) -> &'static str {
        match self {
            Backend::Wasm => "wasm",
            Backend::WasmSwitch => "wasm-switch",
            Backend::Native => "native",
        }
    }

    /// The backend of this name.
    pub fn named(name: &str) -> Option<Backend> {
        Backend::ALL.into_iter().find(|b| b.name() == name)
    }
}

/// A program's module as `backend` starts from: [`prepare`]d for Wasm
/// and native code, which lower tail-resumptive and aborting handlers
/// themselves; and optimized, with its handlers kept, for Wasm with stack
/// switching.
pub fn prepare_for(mut m: Module, backend: Backend) -> Result<Module, String> {
    match backend {
        Backend::Wasm | Backend::Native => prepare(m),
        Backend::WasmSwitch => {
            conduit_opt::optimize_module(&mut m);
            Ok(m)
        }
    }
}

/// A way of compiling a module's memory management, after the optimizer:
/// the ones the benchmarks compare, in [`CONFIGS`], each adding one thing
/// to the one before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub name: &'static str,
    /// Counts inserted; else the module never frees.
    pub counts: bool,
    /// Borrowed parameters inferred; else every one is owned.
    pub borrows: bool,
    pub reuse: bool,
    /// Drops specialized.
    pub specialize: bool,
    /// Cycles collected, which is up to whoever runs the module.
    pub cycles: bool,
}

/// The ways of compiling that the benchmarks compare.
pub const CONFIGS: [Config; 6] = {
    let none = Config {
        name: "optimized",
        counts: false,
        borrows: false,
        reuse: false,
        specialize: false,
        cycles: false,
    };
    let counts = Config {
        name: "counts",
        counts: true,
        ..none
    };
    let borrows = Config {
        name: "+borrows",
        borrows: true,
        ..counts
    };
    let reuse = Config {
        name: "+reuse",
        reuse: true,
        ..borrows
    };
    let specialized = Config {
        name: "+specialized",
        specialize: true,
        ..reuse
    };
    let cycles = Config {
        name: "+cycles",
        cycles: true,
        ..specialized
    };
    [none, counts, borrows, reuse, specialized, cycles]
};

impl Config {
    /// The config of this name.
    pub fn named(name: &str) -> Option<Config> {
        CONFIGS.iter().copied().find(|c| c.name == name)
    }

    /// Compiles `m`, which is optimized already, this way. Collecting
    /// cycles is the runner's to do.
    pub fn apply(&self, m: &mut Module) -> Result<(), String> {
        if !self.counts {
            return Ok(());
        }
        if self.reuse {
            insert_reuse(m);
        }
        if self.borrows {
            insert_counts(m)?;
        } else {
            insert_counts_with(m, &Borrows::default())?;
        }
        if self.specialize {
            specialize_drops(m);
        }
        Ok(())
    }
}
