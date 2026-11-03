//! What the test programs in `tests/programs` can't check: whether a pass
//! reports a change, and functions too long to write out. The programs
//! check what the passes do (see `tests/programs.rs`).

use conduit_ir::text::parse;
use conduit_ir::*;

use crate::dce::dce;
use crate::optimize_module;

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
