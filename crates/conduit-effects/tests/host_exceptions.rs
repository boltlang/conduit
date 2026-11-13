//! What the host throws, after evidence passing, in a module where `throw`
//! has a link: each extern with `throw` is called through a wrapper that
//! catches it and raises it (#65), so a handler for `throw` catches it,
//! even after a yield to a general handler outside it, and a `raise` that
//! nothing handles goes to the host. In the evaluator, with counts, and
//! with what the source gives to compare with.

use conduit_eval::{Arrays, ErrorKind, Host, HostError, Machine, Value};
use conduit_ir::verify::verify_module;
use conduit_ir::{Extern, Module, Op};

const MODULE: &str = r#"
data Box {
  (i32)
}

#[link(emu, name = "exceptions")]
effect throw

effect Ask {
  () -> (i32)
}

effect Log {
  (i32) -> ()
}

// Throws host 7 for a negative number, and gives 10 times it otherwise.
#[link(emu)]
extern parse(i32) -> (i32) does throw

#[link(emu)]
extern is_seven(host) -> (i32)

// Gives -1 for host 7, and -2 for anything else.
handler catch for throw env() ops(caught) ret fine

// The same, in code that asks, or logs.
handler catch_ask for throw does Ask env() ops(caught_ask) ret fine
handler catch_log for throw does Log env() ops(caught_log) ret fine

fn caught_ask(%e: nullable host, %k: cont() -> (i32) does Ask) -> (i32) does Ask {
  let %r: i32 = call what(%e)
  jump ret(%r)
}

fn caught_log(%e: nullable host, %k: cont() -> (i32) does Log) -> (i32) does Log {
  let %r: i32 = call what(%e)
  jump ret(%r)
}

fn caught(%e: nullable host, %k: cont() -> (i32)) -> (i32) {
  let %r: i32 = call what(%e)
  jump ret(%r)
}

fn what(%e: nullable host) -> (i32) {
  let %h: host = host_unwrap(%e)
  let %seven: i32 = call_extern is_seven(%h)
  let %r: i32 = sub(-2i32, %seven)
  jump ret(%r)
}

fn fine(%r: i32) -> (i32) {
  jump ret(%r)
}

// Gives -100 for what it catches.
handler outer_catch for throw env() ops(outer_caught) ret fine

fn outer_caught(%e: nullable host, %k: cont() -> (i32)) -> (i32) {
  jump ret(-100i32)
}

// Resumes with -1, then adds 1000: a general handler, which evidence
// passing yields to.
handler asker for Ask env() ops(ask) ret fine

fn ask(%k: cont(i32) -> (i32)) -> (i32) {
  let %x: i32 = resume(%k, -1i32)
  let %r: i32 = add(%x, 1000i32)
  jump ret(%r)
}

// The same, around code that throws.
handler asker_throwing for Ask does throw env() ops(ask_throwing) ret fine

fn ask_throwing(%k: cont(i32) -> (i32) does throw) -> (i32) does throw {
  let %x: i32 = resume(%k, -1i32)
  let %r: i32 = add(%x, 1000i32)
  jump ret(%r)
}

// Parses what it's given, where it's performed: tail-resumptive.
handler logger for Log does throw env() ops(log) ret fine

fn log(%n: i32, %k: cont() -> (i32) does throw) -> (i32) does throw {
  let %p: i32 = call_extern parse(%n)
  let %r: i32 = resume(%k)
  jump ret(%r)
}

fn parse_boxed(%n: i32) -> (i32) does throw {
  let %b: mut Box = new Box.0(%n)
  let %r: i32 = call_extern parse(%n)
  let %x: i32 = get Box.0.0(%b)
  let %s: i32 = add(%r, %x)
  jump ret(%s)
}

fn main(%n: i32) -> (i32) {
  let %b: imm fn() -> (i32) does throw = closure parse_boxed(%n)
  let %r: i32 = handle catch(%b)
  jump ret(%r)
}

// Asks, then parses what it got, so that it throws after the yield.
fn ask_then_parse() -> (i32) does Ask + throw {
  let %b: mut Box = new Box.0(5i32)
  let %n: i32 = perform Ask.0()
  let %r: i32 = call parse_boxed(%n)
  let %x: i32 = get Box.0.0(%b)
  let %s: i32 = add(%r, %x)
  jump ret(%s)
}

fn caught_inside() -> (i32) does Ask {
  let %b: imm fn() -> (i32) does Ask + throw = closure ask_then_parse()
  let %r: i32 = handle catch_ask(%b)
  jump ret(%r)
}

// The handler for throw is inside the one for Ask, so it must catch what
// the code after the yield throws: -1 + 1000.
fn after_a_yield() -> (i32) {
  let %b: imm fn() -> (i32) does Ask = closure caught_inside()
  let %r: i32 = handle asker(%b)
  jump ret(%r)
}

fn log_it() -> (i32) does Log {
  let = perform Log.0(-3i32)
  jump ret(0i32)
}

fn logged_inside() -> (i32) does Log {
  let %b: imm fn() -> (i32) does Log = closure log_it()
  let %r: i32 = handle catch_log(%b)
  jump ret(%r)
}

fn logged() -> (i32) does throw {
  let %b: imm fn() -> (i32) does Log + throw = closure logged_inside()
  let %r: i32 = handle logger(%b)
  jump ret(%r)
}

// The clause for Log runs outside `catch`, though where it's performed,
// so what it throws goes to `outer_catch`: -100.
fn clause_outside() -> (i32) {
  let %b: imm fn() -> (i32) does throw = closure logged()
  let %r: i32 = handle outer_catch(%b)
  jump ret(%r)
}

// What nothing catches goes to the host.
fn uncaught(%n: i32) -> (i32) does throw {
  let %r: i32 = call parse_boxed(%n)
  jump ret(%r)
}

fn uncaught_after_a_yield() -> (i32) does throw {
  let %b: imm fn() -> (i32) does Ask + throw = closure ask_then_parse()
  let %r: i32 = handle asker_throwing(%b)
  jump ret(%r)
}
"#;

struct Throws;

impl Host for Throws {
    fn call(
        &mut self,
        name: &str,
        _: &Extern,
        args: &[Value],
        _: &mut Arrays<'_, '_>,
    ) -> Result<Vec<Value>, HostError> {
        match (name, args) {
            ("parse", [Value::I32(n)]) if *n < 0 => Err(HostError::Throw(Value::Host(7))),
            ("parse", [Value::I32(n)]) => Ok(vec![Value::I32(n * 10)]),
            ("is_seven", [Value::Host(h)]) => Ok(vec![Value::I32(i32::from(*h == 7))]),
            _ => Err("no such function".into()),
        }
    }
}

/// The module as written, lowered, lowered then optimized, and lowered
/// then counted.
fn modules() -> Vec<(&'static str, Module)> {
    let source: Module = conduit_ir::text::parse(MODULE).unwrap();
    verify_module(&source).unwrap();
    let mut lowered = source.clone();
    conduit_effects::lower_effects(&mut lowered).unwrap();
    if let Err(errors) = verify_module(&lowered) {
        let errors: Vec<String> = errors.iter().map(|e| e.named(&lowered)).collect();
        panic!(
            "{}\n\n{}",
            errors.join("\n"),
            conduit_ir::text::print(&lowered)
        );
    }
    // `catch_extern` prints and parses back.
    let printed = conduit_ir::text::print(&lowered);
    let reparsed = conduit_ir::text::parse(&printed).unwrap();
    assert_eq!(conduit_ir::text::print(&reparsed), printed);
    let mut optimized = lowered.clone();
    conduit_opt::optimize_module(&mut optimized);
    verify_module(&optimized).unwrap();
    let mut counted = lowered.clone();
    conduit_opt::ownership::insert_counts(&mut counted).unwrap();
    vec![
        ("source", source),
        ("lowered", lowered),
        ("optimized", optimized),
        ("counted", counted),
    ]
}

fn run(name: &str, m: &Module, f: &str, args: Vec<Value>) -> Result<Vec<Value>, ErrorKind> {
    let id = conduit_ir::text::find_func(m, f).unwrap();
    let mut machine = Machine::new(m, Throws);
    machine.counting = name == "counted";
    match machine.call(id, args) {
        Ok(values) => {
            if machine.counting {
                let leaks = machine.leaks(&values);
                assert!(
                    leaks.undropped.is_empty() && leaks.overdropped.is_empty(),
                    "{f}: {leaks:?}"
                );
            }
            Ok(values)
        }
        Err(e) => Err(e.kind),
    }
}

#[test]
fn lowering_catches_at_the_externs_that_throw() {
    let (_, lowered) = &modules()[1];
    for f in lowered.funcs.values() {
        for l in f.body.all_lets() {
            if let Op::CallExtern(e) = &l.op {
                assert!(
                    !lowered.externs[e].sig.row.throws(),
                    "a call of an extern that throws isn't caught"
                );
            }
        }
    }
    let catching = (lowered.funcs.values())
        .flat_map(|f| f.body.all_lets())
        .filter(|l| matches!(l.op, Op::CatchExtern(_)))
        .count();
    assert_eq!(catching, 1, "one wrapper for parse");
}

#[test]
fn a_handler_for_throw_catches_what_the_host_throws() {
    for (name, m) in modules() {
        let main = |n| run(name, &m, "main", vec![Value::I32(n)]);
        assert_eq!(main(2), Ok(vec![Value::I32(22)]), "{name}");
        assert_eq!(main(-1), Ok(vec![Value::I32(-3)]), "{name}");
    }
}

#[test]
fn it_catches_after_a_yield_to_a_handler_outside() {
    for (name, m) in modules() {
        let got = run(name, &m, "after_a_yield", vec![]);
        assert_eq!(got, Ok(vec![Value::I32(997)]), "{name}");
    }
}

#[test]
fn what_a_clause_throws_goes_past_handlers_inside_its_own() {
    for (name, m) in modules() {
        let got = run(name, &m, "clause_outside", vec![]);
        assert_eq!(got, Ok(vec![Value::I32(-100)]), "{name}");
    }
}

#[test]
fn what_nothing_catches_goes_to_the_host() {
    for (name, m) in modules() {
        let got = run(name, &m, "uncaught", vec![Value::I32(-1)]);
        assert_eq!(got, Err(ErrorKind::Uncaught("host 7".into())), "{name}");
        let got = run(name, &m, "uncaught", vec![Value::I32(1)]);
        assert_eq!(got, Ok(vec![Value::I32(11)]), "{name}");
        let got = run(name, &m, "uncaught_after_a_yield", vec![]);
        assert_eq!(got, Err(ErrorKind::Uncaught("host 7".into())), "{name}");
    }
}
