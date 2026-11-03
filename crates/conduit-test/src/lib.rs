//! Runs the test programs in `tests/programs` against each part of Conduit.
//!
//! A test program is a `.conduit` file in the text format, with checks in
//! comments that start with `//#`. See `tests/programs/README.md` for the
//! checks. Each crate that tests itself on these programs has a test
//! target that calls [`main`] with the checks it understands, so
//! `cargo test` shows one test per file and `cargo test fac` runs only the
//! files whose path contains `fac`.
//!
//! The same `run` checks are run by the evaluator, by the Wasm backend and
//! after optimization, so every backend has to agree with what a program
//! is written to give.

mod checks;
pub mod eval;

use std::fs;
use std::path::{Path, PathBuf};

use conduit_ir::{Atom, FuncId, Lit, Module};
use libtest_mimic::{Arguments, Failed, Trial};

pub use checks::{Check, CheckKind, Expect};

/// A test program.
#[derive(Debug, Clone)]
pub struct Program {
    pub path: PathBuf,
    /// The path relative to `tests/programs`, without the extension.
    pub name: String,
    pub text: String,
    pub checks: Vec<Check>,
}

/// What running a function gave.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Values(Vec<Lit>),
    /// A run-time error a program can cause on purpose, like a division by
    /// zero or `unreachable`.
    Trap(String),
}

/// A `run` check, with where it is.
pub struct Run<'a> {
    pub line: u32,
    pub func: FuncId,
    pub args: &'a [Lit],
    pub expect: &'a Expect,
}

/// The root of the workspace.
fn workspace_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    dir.canonicalize().unwrap_or(dir)
}

/// `path` relative to the workspace, for messages.
fn relative(path: &Path) -> String {
    let short = path.strip_prefix(workspace_dir()).unwrap_or(path);
    short.display().to_string()
}

/// The directory holding the test programs.
pub fn programs_dir() -> PathBuf {
    workspace_dir().join("tests/programs")
}

impl Program {
    /// The path relative to the workspace, for messages.
    pub fn shown(&self) -> String {
        relative(&self.path)
    }

    /// The module the program's text describes.
    pub fn module(&self) -> Result<Module, String> {
        conduit_ir::text::parse(&self.text).map_err(|e| format!("{}:{e}", self.shown()))
    }

    pub fn has(&self, f: impl Fn(&CheckKind) -> bool) -> bool {
        self.checks.iter().any(|c| f(&c.kind))
    }

    pub fn runs(&self) -> impl Iterator<Item = Run<'_>> {
        self.checks.iter().filter_map(|c| match &c.kind {
            CheckKind::Run { func, args, expect } => Some(Run {
                line: c.line,
                func: *func,
                args,
                expect,
            }),
            _ => None,
        })
    }

    /// The messages of the checks `f` picks.
    pub fn messages(&self, f: impl Fn(&CheckKind) -> Option<&str>) -> Vec<&str> {
        self.checks.iter().filter_map(|c| f(&c.kind)).collect()
    }

    /// Where the check on `line` is, for messages.
    pub fn at(&self, line: u32) -> String {
        format!("{}:{line}", self.shown())
    }

    /// Checks `actual` against the snapshot next to the program with
    /// extension `ext`, like `fac.wat` for `fac.conduit`. With the
    /// environment variable `BLESS=1`, writes `actual` to it instead.
    pub fn snapshot(&self, ext: &str, actual: &str) -> Result<(), String> {
        let path = self.path.with_extension(ext);
        if std::env::var_os("BLESS").is_some_and(|v| v == "1") {
            return fs::write(&path, actual).map_err(|e| format!("{}: {e}", relative(&path)));
        }
        let Ok(expected) = fs::read_to_string(&path) else {
            return Err(format!(
                "{} doesn't exist yet: run the tests with BLESS=1 to create it",
                relative(&path)
            ));
        };
        if expected == actual {
            return Ok(());
        }
        Err(format!(
            "{} doesn't match (run the tests with BLESS=1 to update it):\n{}",
            relative(&path),
            diff(&expected, actual)
        ))
    }
}

impl Run<'_> {
    /// Compares what running the function gave with what the check
    /// expects.
    pub fn check(&self, program: &Program, got: Result<Outcome, String>) -> Result<(), String> {
        let call = format!(
            "{}({})",
            self.func,
            self.args.iter().map(lit).collect::<Vec<_>>().join(", ")
        );
        let at = program.at(self.line);
        match (self.expect, got) {
            (_, Err(e)) => Err(format!("{at}: {call} failed: {e}")),
            (Expect::Trap, Ok(Outcome::Trap(_))) => Ok(()),
            (Expect::Trap, Ok(Outcome::Values(vals))) => Err(format!(
                "{at}: {call} should trap, but gave {}",
                lits(&vals)
            )),
            (Expect::Values(_), Ok(Outcome::Trap(e))) => Err(format!("{at}: {call} trapped: {e}")),
            (Expect::Values(expected), Ok(Outcome::Values(got))) => {
                // Compare floats by their bits, so NaNs and -0.0 count.
                if *expected == got {
                    Ok(())
                } else {
                    Err(format!(
                        "{at}: {call} should give {}, but gave {}",
                        lits(expected),
                        lits(&got)
                    ))
                }
            }
        }
    }
}

fn lit(l: &Lit) -> String {
    Atom::Lit(*l).to_string()
}

fn lits(ls: &[Lit]) -> String {
    match ls {
        [one] => lit(one),
        _ => format!("({})", ls.iter().map(lit).collect::<Vec<_>>().join(", ")),
    }
}

/// The lines that differ, marked `-` for `expected` and `+` for `actual`,
/// with the line numbers of `expected`.
fn diff(expected: &str, actual: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    // The longest common subsequence, by dynamic programming over suffixes.
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = String::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out += &format!("{:>4} + {}\n", i + 1, b[j]);
            j += 1;
        } else {
            out += &format!("{:>4} - {}\n", i + 1, a[i]);
            i += 1;
        }
    }
    out
}

/// Reads every test program.
pub fn programs() -> Vec<Result<Program, (String, String)>> {
    let root = programs_dir();
    let mut paths = Vec::new();
    collect(&root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path)
                .map_err(|e| (name.clone(), format!("{}: {e}", relative(&path))))?;
            let checks = checks::parse(&text)
                .map_err(|(line, e)| (name.clone(), format!("{}:{line}: {e}", relative(&path))))?;
            Ok(Program {
                path,
                name,
                text,
                checks,
            })
        })
        .collect()
}

/// The `.conduit` files under `dir`, leaving out snapshots like
/// `fac.opt.conduit`.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name
            .strip_suffix(".conduit")
            .is_some_and(|stem| !stem.contains('.'))
        {
            out.push(path);
        }
    }
}

/// Runs `test` on every program that `applies` to, as one test each, and
/// exits. Programs it doesn't apply to are listed as ignored. A program
/// whose checks don't parse fails in every crate.
pub fn main(applies: fn(&Program) -> bool, test: fn(&Program) -> Result<(), String>) -> ! {
    let args = Arguments::from_args();
    let trials = programs()
        .into_iter()
        .map(|p| match p {
            Ok(p) => {
                let ignored = !applies(&p);
                Trial::test(p.name.clone(), move || test(&p).map_err(Failed::from))
                    .with_ignored_flag(ignored)
            }
            Err((name, e)) => Trial::test(name, move || Err(Failed::from(e))),
        })
        .collect();
    libtest_mimic::run(&args, trials).exit()
}
