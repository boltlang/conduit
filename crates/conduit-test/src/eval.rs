//! Running `run` checks with the evaluator.

use conduit_eval::{ErrorKind, Host, Leaks, Machine, Value};
use conduit_ir::{Extern, ExternId, Lit, Module};

use crate::{CheckKind, Outcome, Program};

/// How many steps a test program may take in the evaluator, so a program
/// that loops forever fails instead of hanging.
const FUEL: u64 = 100_000_000;

/// Runs every `run` check of `p` on `m` with the evaluator. `m` is usually
/// `p`'s module, but can be one derived from it, like after optimization.
pub fn check_runs(p: &Program, m: &Module) -> Result<(), String> {
    check_runs_then(p, m, |_| Ok(()))
}

/// [`check_runs`], then `after` on the machine of each run, whose heap
/// holds everything the run made.
pub fn check_runs_then(
    p: &Program,
    m: &Module,
    after: impl Fn(&Machine<'_, Module, TestHost>) -> Result<(), String>,
) -> Result<(), String> {
    runs(p, m, false, |_| Ok(()), after)
}

/// [`check_runs`], with reference counting on if `p` has a `counted`
/// check, in which case every run must free everything but its results.
/// Only for `p`'s own module: passes don't keep counts right.
pub fn check_runs_counted(p: &Program, m: &Module) -> Result<(), String> {
    let counting = p.has(|k| matches!(k, CheckKind::Counted));
    let all_freed = |leaks: &Leaks| match leaks.is_empty() {
        true => Ok(()),
        false => Err(leaks.to_string()),
    };
    runs(p, m, counting, all_freed, |_| Ok(()))
}

/// [`check_runs`] with reference counting on, then `leaks` on what each
/// run that gave results left behind, with its results as roots.
pub fn check_runs_with_counts(
    p: &Program,
    m: &Module,
    leaks: impl Fn(&Leaks) -> Result<(), String>,
) -> Result<(), String> {
    runs(p, m, true, leaks, |_| Ok(()))
}

fn runs(
    p: &Program,
    m: &Module,
    counting: bool,
    check_leaks: impl Fn(&Leaks) -> Result<(), String>,
    after: impl Fn(&Machine<'_, Module, TestHost>) -> Result<(), String>,
) -> Result<(), String> {
    for run in p.runs(m)? {
        let mut machine = Machine::new(m, TestHost);
        machine.fuel = Some(FUEL);
        machine.counting = counting;
        let args = run.args.iter().map(|&l| Value::from(l)).collect();
        let mut leaks = None;
        let got = match machine.call(run.func, args) {
            Ok(values) => {
                leaks = Some(machine.leaks(&values));
                values
                    .into_iter()
                    .map(lit)
                    .collect::<Result<_, _>>()
                    .map(Outcome::Values)
            }
            Err(e) if is_trap(&e.kind) => Ok(Outcome::Trap(e.to_string())),
            Err(e) => Err(e.to_string()),
        };
        run.check(p, got)?;
        if let Some(leaks) = leaks.filter(|_| counting) {
            check_leaks(&leaks).map_err(|e| format!("{}: {}: {e}", p.at(run.line), run.name))?;
        }
        after(&machine)?;
    }
    Ok(())
}

/// Errors a program can cause on purpose, which Wasm traps on too.
fn is_trap(kind: &ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::IntegerOverflow
            | ErrorKind::InvalidConversion
            | ErrorKind::OutOfBounds { .. }
            | ErrorKind::Trap
            | ErrorKind::ContinuationResumedTwice
            | ErrorKind::Unhandled(_)
    )
}

fn lit(v: Value) -> Result<Lit, String> {
    Ok(match v {
        Value::I8(n) => Lit::I8(n),
        Value::I16(n) => Lit::I16(n),
        Value::I32(n) => Lit::I32(n),
        Value::I64(n) => Lit::I64(n),
        Value::F32(x) => Lit::f32(x),
        Value::F64(x) => Lit::f64(x),
        Value::Obj(_) => {
            return Err("gave an object, but a test can only check scalars".into());
        }
        Value::Unset => return Err("gave an unset array element".into()),
    })
}

/// The host functions test programs can import, from the module `"test"`.
/// Every backend's runner provides the same ones.
///
/// - `double(i32) -> i32` gives twice its argument.
pub struct TestHost;

impl Host for TestHost {
    fn call(&mut self, _: ExternId, decl: &Extern, args: &[Value]) -> Result<Vec<Value>, String> {
        match (&*decl.module, &*decl.name, args) {
            ("test", "double", [Value::I32(n)]) => Ok(vec![Value::I32(n.wrapping_mul(2))]),
            _ => Err(format!(
                "no test host function {:?} {:?} taking these arguments",
                decl.module, decl.name
            )),
        }
    }
}
