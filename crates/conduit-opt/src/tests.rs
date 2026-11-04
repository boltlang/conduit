//! What the test programs in `tests/programs` can't check: whether a pass
//! reports a change, and functions too long to write out. The programs
//! check what the passes do (see `tests/programs.rs`).

use conduit_ir::text::parse;
use conduit_ir::*;

use crate::dce::dce;
use crate::inline::inline;
use crate::optimize_module;
use crate::promote::promote;

const TWO_DEAD_ADDS: &str = "
fn fn0(%0: i32) -> (i32) {
  let %1: i32 = add(%0, 1i32)
  jump ret(%0)
}

fn fn1(%0: i32) -> (i32) {
  let %1: i32 = add(%0, 1i32)
  jump ret(%0)
}
";

#[test]
fn reports_no_change_when_nothing_is_dead() {
    let m = parse(
        "fn fn0(%0: i32) -> (i32) {
           let %1: i32 = add(%0, 1i32)
           jump ret(%1)
         }",
    )
    .unwrap();
    let mut f = m.funcs[&FuncId(0)].clone();
    assert!(!dce(&mut f));
    assert_eq!(f, m.funcs[&FuncId(0)]);
}

#[test]
fn optimizes_every_function_of_a_module() {
    let mut m = parse(TWO_DEAD_ADDS).unwrap();
    assert!(optimize_module(&mut m));
    for f in m.funcs.values() {
        assert_eq!(f.body.lets, []);
    }
    assert!(!optimize_module(&mut m));
}

/// A long list of `let`s doesn't overflow the stack.
#[test]
fn long_functions() {
    let mut f = Func::new(vec![Ty::I32], vec![Ty::I32]);
    let x = Atom::Var(f.params[0]);
    let ret = Tail::Jump {
        target: Label::RET,
        args: vec![x.clone()],
        source: None,
    };
    let lets = (0..200_000)
        .map(|_| Let {
            binds: vec![f.new_var(Ty::I32)],
            op: Op::Prim(Prim::Add),
            args: vec![x.clone(), Atom::Lit(Lit::I32(1))],
            source: None,
        })
        .collect();
    f.body = Block {
        lets,
        tail: ret.clone(),
    };
    assert!(dce(&mut f));
    assert_eq!(f.body, Block::from(ret));
}

/// Without refs, or with only refs that escape, promotion changes nothing.
#[test]
fn promote_reports_no_change() {
    let m = parse(
        "fn fn0(%0: ref i32) -> (i32) {
           let %1: i32 = ref_get(%0)
           jump ret(%1)
         }

         fn fn1() -> (ref i32) {
           let %0: ref i32 = ref_new(1i32)
           jump ret(%0)
         }",
    )
    .unwrap();
    for f in m.funcs.values() {
        let mut g = f.clone();
        assert!(!promote(&mut g));
        assert_eq!(&g, f);
    }
}

/// A ref of a continuation stays, so that the verifier doesn't count
/// reads on different paths as uses of the continuation.
#[test]
fn refs_of_continuations_stay() {
    let m = parse(
        "fn fn0(%0: cont() -> ()) -> () {
           let %1: ref cont() -> () = ref_new(%0)
           let %2: cont() -> () = ref_get(%1)
           let = resume(%2)
           jump ret()
         }",
    )
    .unwrap();
    let mut f = m.funcs[&FuncId(0)].clone();
    assert!(!promote(&mut f));
}

#[test]
fn inlining_reports_whether_it_changed_anything() {
    let mut m = parse(
        "fn fn0(%0: i32) -> (i32) {
           let %1: i32 = add(%0, 1i32)
           jump ret(%1)
         }

         fn fn1(%0: i32) -> (i32) {
           let %1: i32 = call fn0(%0)
           jump ret(%1)
         }",
    )
    .unwrap();
    let mut f = m.funcs[&FuncId(1)].clone();
    assert!(inline(&m, FuncId(1), &mut f));
    m.funcs.insert(FuncId(1), f.clone());
    conduit_ir::verify::verify_module(&m).unwrap();
    assert!(!inline(&m, FuncId(1), &mut f));
    let mut g = m.funcs[&FuncId(0)].clone();
    assert!(!inline(&m, FuncId(0), &mut g));
}
