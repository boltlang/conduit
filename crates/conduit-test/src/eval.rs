//! Running `run` checks with the evaluator.

use conduit_eval::{Arrays, ErrorKind, Host, Leaks, Machine, Value};
use conduit_ir::{Extern, Lit, Module};

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

/// [`check_runs`] with reference counting on, and cycles collected, often,
/// then `leaks` on what each run that gave results left behind, with its
/// results as roots, after a last collection.
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
        // Cycles too, collected often, so that collections run in the
        // middle of the program.
        machine.cycles = counting;
        machine.cycle_buffer = 8;
        let args = run.args.iter().map(|&l| Value::from(l)).collect();
        let mut leaks = None;
        let got = match machine.call(run.func, args) {
            Ok(values) => {
                machine.collect_cycles();
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

/// The host functions test programs can import, with an
/// `#[link(emu, ..)]`. Every backend's runner provides the same ones: the
/// Wasm runner as imports from the module `"test"`, which it links the
/// externs to (see [`crate::link_for_wasm`]).
///
/// - `double(i32) -> i32` gives twice its argument.
///
/// And these, meant for the flat ABI, where Wasm passes arrays through
/// memory:
///
/// - `sum(imm [i8]) -> (i32)` adds up the bytes, as signed.
/// - `fill(mut [i8], i32) -> ()` sets every byte to the low byte of the
///   second argument.
/// - `gather(imm [imm [i8]]) -> (i32, i32)` gives how many buffers there
///   are and the sum of all their bytes.
/// - `squares(#[nolen] mut [i32], i32 n) -> ()` sets elements 0 to n - 1 to
///   their index squared.
/// - `scale(i32) -> (i32, i64)` gives 0 and its argument times 10^12.
/// - `byte(i8) -> (i32)` gives its argument zero-extended, as the flat ABI
///   passes it.
pub struct TestHost;

impl Host for TestHost {
    fn call(
        &mut self,
        name: &str,
        _: &Extern,
        args: &[Value],
        arrays: &mut Arrays<'_, '_>,
    ) -> Result<Vec<Value>, String> {
        let bytes = |arrays: &Arrays<'_, '_>, v: &Value| -> Result<i32, String> {
            let elems = arrays.get(v).ok_or("not an array")?;
            elems.iter().try_fold(0i32, |sum, e| match e {
                Value::I8(b) => Ok(sum.wrapping_add(i32::from(*b))),
                _ => Err("not a byte".to_string()),
            })
        };
        match (name, args) {
            ("double", [Value::I32(n)]) => Ok(vec![Value::I32(n.wrapping_mul(2))]),
            ("sum", [a]) => Ok(vec![Value::I32(bytes(arrays, a)?)]),
            ("fill", [a, Value::I32(b)]) => {
                for e in arrays.get_mut(a).ok_or("not a mut array")? {
                    *e = Value::I8(*b as i8);
                }
                Ok(vec![])
            }
            ("gather", [list]) => {
                let bufs = arrays.get(list).ok_or("not an array")?.to_vec();
                let total = bufs.iter().try_fold(0i32, |sum, b| {
                    Ok::<_, String>(sum.wrapping_add(bytes(arrays, b)?))
                })?;
                Ok(vec![Value::I32(bufs.len() as i32), Value::I32(total)])
            }
            ("squares", [a, Value::I32(n)]) => {
                let elems = arrays.get_mut(a).ok_or("not a mut array")?;
                let n = usize::try_from(*n).map_err(|e| e.to_string())?;
                for (i, e) in elems.iter_mut().take(n).enumerate() {
                    *e = Value::I32((i * i) as i32);
                }
                Ok(vec![])
            }
            ("scale", [Value::I32(n)]) => Ok(vec![
                Value::I32(0),
                Value::I64(i64::from(*n) * 1_000_000_000_000),
            ]),
            ("byte", [Value::I8(b)]) => Ok(vec![Value::I32(i32::from(*b as u8))]),
            _ => Err(format!(
                "no test host function {name:?} taking these arguments"
            )),
        }
    }
}
