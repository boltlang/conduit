//! Each test builds a function, runs a pass, and checks that the result
//! still verifies and computes the same as before (with the evaluator),
//! and which ops and join points are left.

use conduit_eval::{Machine, NoHost, Value};
use conduit_ir::verify::verify_module;
use conduit_ir::*;

use crate::dce::dce;
use crate::optimize_module;

use Ty::I32;

const F: FuncId = FuncId(0);

fn v(x: Var) -> Atom {
    Atom::Var(x)
}

fn int(n: i32) -> Atom {
    Atom::Lit(Lit::I32(n))
}

fn let_(binds: &[Var], op: Op, args: Vec<Atom>, body: Block) -> Block {
    (body).with_let(Let {
        binds: binds.to_vec(),
        op,
        args,
        source: None,
    })
}

fn prim(bind: Var, p: Prim, args: Vec<Atom>, body: Block) -> Block {
    let_(&[bind], Op::Prim(p), args, body)
}

fn jump(target: Label, args: Vec<Atom>) -> Block {
    Block::from(Tail::Jump {
        target,
        args,
        source: None,
    })
}

fn ret(args: Vec<Atom>) -> Block {
    jump(Label::RET, args)
}

fn join(rec: bool, defs: Vec<(Label, Vec<Var>, Block)>, body: Block) -> Block {
    Block::from(Tail::Join {
        rec,
        defs: defs
            .into_iter()
            .map(|(label, params, body)| JoinDef {
                label,
                params,
                body,
            })
            .collect(),
        body: Box::new(body),
    })
}

fn module(funcs: Vec<(FuncId, Func)>) -> Module {
    let mut m = Module::default();
    m.funcs.extend(funcs);
    m
}

/// The ops left in a block, from top to bottom.
fn ops(b: &Block) -> Vec<Op> {
    let mut out = Vec::new();
    walk(b, &mut |b| out.extend(b.lets.iter().map(|l| l.op.clone())));
    out
}

/// The join points left in a block.
fn labels(b: &Block) -> Vec<Label> {
    let mut out = Vec::new();
    walk(b, &mut |b| {
        if let Tail::Join { defs, .. } = &b.tail {
            out.extend(defs.iter().map(|d| d.label));
        }
    });
    out
}

fn walk(b: &Block, f: &mut impl FnMut(&Block)) {
    f(b);
    match &b.tail {
        Tail::Join { defs, body, .. } => {
            walk(body, f);
            for d in defs {
                walk(&d.body, f);
            }
        }
        Tail::Case { arms, default, .. } => {
            for a in arms {
                walk(&a.body, f);
            }
            if let Some(d) = default {
                walk(d, f);
            }
        }
        Tail::Jump { .. } | Tail::Unreachable => {}
    }
}

/// Runs `dce` on function `F` of `m`, checks that the result verifies and
/// gives the same results as before for each of `inputs`, and returns it.
fn check(m: &Module, inputs: &[Vec<Value>]) -> (bool, Func) {
    verify_module(m).unwrap_or_else(|e| panic!("the test's IR doesn't verify: {e:#?}"));
    let mut after = m.clone();
    let f = after.funcs.get_mut(&F).unwrap();
    let changed = dce(f);
    let f = f.clone();
    verify_module(&after).unwrap_or_else(|e| panic!("the IR after dce doesn't verify: {e:#?}"));
    for args in inputs {
        let expected = Machine::new(m, NoHost).call(F, args.clone());
        let got = Machine::new(&after, NoHost).call(F, args.clone());
        assert_eq!(
            got.map_err(|e| e.to_string()),
            expected.map_err(|e| e.to_string()),
            "different results for {args:?}"
        );
    }
    (changed, f)
}

fn i32s(xs: &[i32]) -> Vec<Value> {
    xs.iter().map(|&x| Value::I32(x)).collect()
}

/// The example from issue #5.
#[test]
fn drops_pure_lets_and_unused_joins_but_keeps_traps() {
    // join unused() = jump ret(0)
    // in let a = add(x, 1)
    //    let b = div(x, y)
    //    let c = mul(x, 2)
    //    jump ret(c)
    let mut f = Func::new(vec![I32, I32], vec![I32]);
    let [x, y] = [f.params[0], f.params[1]];
    let unused = f.new_label();
    let [a, b, c] = [(); 3].map(|_| f.new_var(I32));
    let div = Prim::Div(Sign::Signed);
    f.body = join(
        false,
        vec![(unused, vec![], ret(vec![int(0)]))],
        prim(
            a,
            Prim::Add,
            vec![v(x), int(1)],
            prim(
                b,
                div.clone(),
                vec![v(x), v(y)],
                prim(c, Prim::Mul, vec![v(x), int(2)], ret(vec![v(c)])),
            ),
        ),
    );
    // Dividing by zero must still fail after the pass.
    let (changed, f) = check(&module(vec![(F, f)]), &[i32s(&[6, 3]), i32s(&[6, 0])]);
    assert!(changed);
    assert_eq!(ops(&f.body), [Op::Prim(div), Op::Prim(Prim::Mul)]);
    assert_eq!(labels(&f.body), []);
}

/// Dropping a `let` can make the ones above it dead.
#[test]
fn dead_lets_cascade() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let [a, b] = [(); 2].map(|_| f.new_var(I32));
    f.body = prim(
        a,
        Prim::Add,
        vec![v(x), int(1)],
        prim(b, Prim::Mul, vec![v(a), int(2)], ret(vec![v(x)])),
    );
    let (changed, f) = check(&module(vec![(F, f)]), &[i32s(&[4])]);
    assert!(changed);
    assert_eq!(ops(&f.body), []);
}

/// Unused `New`s and closures are pure and go; calls and writes stay.
#[test]
fn keeps_ops_with_effects() {
    let pair = DataId(0);
    let ctor = CtorId {
        data: pair,
        index: 0,
    };
    let (g, h) = (FuncId(1), FuncId(2));
    let mut m = Module::default();
    m.data.insert(
        pair,
        DataDecl {
            ctors: vec![CtorDecl {
                fields: vec![I32, I32],
            }],
        },
    );

    let mut gf = Func::new(vec![I32], vec![I32]);
    let n = gf.params[0];
    gf.body = ret(vec![v(n)]);
    m.funcs.insert(g, gf.clone());
    m.funcs.insert(h, gf);

    let fn_ty = Ty::Fn(
        Box::new(Sig {
            params: vec![],
            results: vec![I32],
        }),
        Mutability::Imm,
    );
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let p = f.new_var(Ty::Data(pair, Mutability::Mut));
    let q = f.new_var(Ty::Data(pair, Mutability::Imm));
    let c = f.new_var(fn_ty);
    let r = f.new_var(Ty::Ref(Box::new(I32)));
    let called = f.new_var(I32);
    f.body = let_(
        &[p],
        Op::New(ctor),
        vec![v(x), v(x)],
        let_(
            &[],
            Op::Set(ctor, 0),
            vec![v(p), int(1)],
            let_(
                &[q],
                Op::New(ctor),
                vec![int(1), int(2)],
                let_(
                    &[c],
                    Op::Closure(h),
                    vec![v(x)],
                    let_(
                        &[r],
                        Op::RefNew,
                        vec![v(x)],
                        let_(&[called], Op::Call(g), vec![v(x)], ret(vec![v(x)])),
                    ),
                ),
            ),
        ),
    );
    m.funcs.insert(F, f);
    let (changed, f) = check(&m, &[i32s(&[3])]);
    assert!(changed);
    // `p` is written to, so its `New` stays: the `Set` uses it.
    assert_eq!(ops(&f.body), [Op::New(ctor), Op::Set(ctor, 0), Op::Call(g)]);
}

/// A recursive join point that only the join point itself jumps to is dead.
#[test]
fn drops_join_points_that_only_jump_to_themselves() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let lp = f.new_label();
    let a = f.new_var(I32);
    f.body = join(
        true,
        vec![(
            lp,
            vec![],
            prim(a, Prim::Add, vec![v(x), int(1)], jump(lp, vec![])),
        )],
        ret(vec![v(x)]),
    );
    let (changed, f) = check(&module(vec![(F, f)]), &[i32s(&[1])]);
    assert!(changed);
    assert_eq!(labels(&f.body), []);
    assert_eq!(ops(&f.body), []);
}

/// In a group of recursive join points, the ones reached from the body
/// stay, even through each other, and the rest go.
#[test]
fn keeps_only_reachable_join_points_of_a_group() {
    // join rec even(k) = case k { 0 => ret 1, _ => jump odd(k - 1) }
    //          odd(k) = case k { 0 => ret 0, _ => jump even(k - 1) }
    //          dead() = jump even(0)
    // in jump even(n)
    let mut f = Func::new(vec![I32], vec![I32]);
    let n = f.params[0];
    let [even, odd, dead] = [(); 3].map(|_| f.new_label());
    let [k1, k1m, k2, k2m] = [(); 4].map(|_| f.new_var(I32));
    let step = |k, km, result, other| {
        Block::from(Tail::Case {
            scrut: v(k),
            arms: vec![Arm {
                tag: Tag::Int(0),
                body: ret(vec![int(result)]),
            }],
            default: Some(Box::new(prim(
                km,
                Prim::Sub,
                vec![v(k), int(1)],
                jump(other, vec![v(km)]),
            ))),
        })
    };
    f.body = join(
        true,
        vec![
            (dead, vec![], jump(even, vec![int(0)])),
            (even, vec![k1], step(k1, k1m, 1, odd)),
            (odd, vec![k2], step(k2, k2m, 0, even)),
        ],
        jump(even, vec![v(n)]),
    );
    let (changed, f) = check(
        &module(vec![(F, f)]),
        &[i32s(&[0]), i32s(&[7]), i32s(&[10])],
    );
    assert!(changed);
    assert_eq!(labels(&f.body), [even, odd]);
}

/// A use in a dead join point doesn't keep a `let` alive.
#[test]
fn uses_in_dead_code_dont_count() {
    // let a = add(x, 1)
    // join dead() = jump ret(a)
    // in jump ret(x)
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let dead = f.new_label();
    let a = f.new_var(I32);
    f.body = prim(
        a,
        Prim::Add,
        vec![v(x), int(1)],
        join(
            false,
            vec![(dead, vec![], ret(vec![v(a)]))],
            ret(vec![v(x)]),
        ),
    );
    let (changed, f) = check(&module(vec![(F, f)]), &[i32s(&[5])]);
    assert!(changed);
    assert_eq!(ops(&f.body), []);
    assert_eq!(labels(&f.body), []);
}

/// Of the definitions of a non-recursive join, only the unused ones go.
#[test]
fn drops_one_definition_of_a_join() {
    // join used(r) = jump ret(r), unused() = jump ret(0)
    // in let s = add(x, 1); jump used(s)
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let [used, unused] = [(); 2].map(|_| f.new_label());
    let [r, s] = [(); 2].map(|_| f.new_var(I32));
    f.body = join(
        false,
        vec![
            (used, vec![r], ret(vec![v(r)])),
            (unused, vec![], ret(vec![int(0)])),
        ],
        prim(s, Prim::Add, vec![v(x), int(1)], jump(used, vec![v(s)])),
    );
    let (changed, f) = check(&module(vec![(F, f)]), &[i32s(&[5])]);
    assert!(changed);
    assert_eq!(labels(&f.body), [used]);
    assert_eq!(ops(&f.body), [Op::Prim(Prim::Add)]);
}

#[test]
fn reports_no_change_when_nothing_is_dead() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let a = f.new_var(I32);
    f.body = prim(a, Prim::Add, vec![v(x), int(1)], ret(vec![v(a)]));
    let before = f.clone();
    let (changed, f) = check(&module(vec![(F, f)]), &[i32s(&[5])]);
    assert!(!changed);
    assert_eq!(f, before);
}

/// A long chain of `let`s doesn't overflow the stack.
#[test]
fn long_functions() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let vars: Vec<Var> = (0..200_000).map(|_| f.new_var(I32)).collect();
    let mut body = ret(vec![v(x)]);
    body.lets = vars
        .iter()
        .map(|&a| Let {
            binds: vec![a],
            op: Op::Prim(Prim::Add),
            args: vec![v(x), int(1)],
            source: None,
        })
        .collect();
    f.body = body;
    assert!(dce(&mut f));
    assert_eq!(f.body, ret(vec![v(x)]));
}

#[test]
fn optimizes_every_function_of_a_module() {
    let mut m = Module::default();
    for id in [FuncId(0), FuncId(1)] {
        let mut f = Func::new(vec![I32], vec![I32]);
        let x = f.params[0];
        let a = f.new_var(I32);
        f.body = prim(a, Prim::Add, vec![v(x), int(1)], ret(vec![v(x)]));
        m.funcs.insert(id, f);
    }
    assert!(optimize_module(&mut m));
    for f in m.funcs.values() {
        assert_eq!(ops(&f.body), []);
    }
    assert!(!optimize_module(&mut m));
}
