use conduit_ir::verify::verify_module;
use conduit_ir::*;

use crate::*;

use Mutability::{Imm, Mut};

const MAIN: FuncId = FuncId(1);
const G: FuncId = FuncId(2);
const H: FuncId = FuncId(3);

/// `data Point = Point(i32, i32)`
const POINT: DataId = DataId(10);
/// `data Pair = Pair(inherit Point, inherit Point)`
const PAIR: DataId = DataId(11);
/// `data List = Nil | Cons(i32, inherit List)`
const LIST: DataId = DataId(12);

fn v(var: Var) -> Atom {
    Atom::Var(var)
}

fn int(n: i32) -> Atom {
    Atom::Lit(Lit::I32(n))
}

fn let_(binds: Vec<Var>, op: Op, args: Vec<Atom>, body: Block) -> Block {
    (body).with_let(Let {
        binds,
        op,
        args,
        source: None,
    })
}

fn prim(bind: Var, p: Prim, args: Vec<Atom>, body: Block) -> Block {
    let_(vec![bind], Op::Prim(p), args, body)
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

fn if_(cond: Var, then: Block, otherwise: Block) -> Block {
    Block::from(Tail::Case {
        scrut: v(cond),
        arms: vec![Arm {
            tag: Tag::Int(1),
            body: then,
        }],
        default: Some(Box::new(otherwise)),
    })
}

fn ctor(data: DataId, index: u32) -> CtorId {
    CtorId { data, index }
}

fn module() -> Module {
    let mut m = Module::default();
    let ctors = |fields: Vec<Vec<Ty>>| DataDecl {
        debug_name: None,
        params: 0,
        effect_params: 0,
        ctors: fields
            .into_iter()
            .map(|fields| CtorDecl { fields })
            .collect(),
    };
    m.data.insert(POINT, ctors(vec![vec![Ty::I32, Ty::I32]]));
    let point = Ty::data(POINT, Mutability::Inherit);
    m.data.insert(PAIR, ctors(vec![vec![point.clone(), point]]));
    let list = Ty::data(LIST, Mutability::Inherit);
    m.data
        .insert(LIST, ctors(vec![vec![], vec![Ty::I32, list]]));
    m
}

/// Verifies the module, then runs `MAIN`.
fn run(m: &Module, args: Vec<Value>) -> Result<Vec<Value>, Error> {
    if let Err(errors) = verify_module(m) {
        panic!("the test program doesn't verify: {errors:#?}");
    }
    Machine::new(m, NoHost).call(MAIN, args)
}

fn run_main(main: Func, args: Vec<Value>) -> Result<Vec<Value>, Error> {
    let mut m = module();
    m.funcs.insert(MAIN, main);
    run(&m, args)
}

/// `sum(n) = 0 + 1 + .. + (n - 1)`, as a loop.
fn sum() -> Func {
    let mut f = Func::new(vec![Ty::I32], vec![Ty::I32]);
    let n = f.params[0];
    let lp = f.new_label();
    let [i, acc, done, i2, acc2] = [(); 5].map(|_| f.new_var(Ty::I32));
    let step = prim(
        i2,
        Prim::Add,
        vec![v(i), int(1)],
        prim(
            acc2,
            Prim::Add,
            vec![v(acc), v(i)],
            jump(lp, vec![v(i2), v(acc2)]),
        ),
    );
    let body = prim(
        done,
        Prim::Ge(Sign::Signed),
        vec![v(i), v(n)],
        if_(done, ret(vec![v(acc)]), step),
    );
    f.body = Block::from(Tail::Join {
        rec: true,
        defs: vec![JoinDef {
            label: lp,
            params: vec![i, acc],
            body,
        }],
        body: Box::new(jump(lp, vec![int(0), int(0)])),
    });
    f
}

#[test]
fn loop_sums() {
    assert_eq!(
        run_main(sum(), vec![Value::I32(10)]),
        Ok(vec![Value::I32(45)])
    );
}

#[test]
fn long_loop_doesnt_grow_the_stack() {
    let mut m = module();
    m.funcs.insert(MAIN, sum());
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    machine.max_frames = 1;
    let result = machine.call(MAIN, vec![Value::I32(1_000_000)]);
    assert_eq!(result, Ok(vec![Value::I32(1_783_293_664)])); // wrapped
}

/// `count(n) = if n == 0 { 0 } else { 1 + count(n - 1) }`: not a tail call.
/// With `tail`, it's `count(n, acc) = if n == 0 { acc } else { count(n - 1, acc + 1) }`.
fn count(tail: bool) -> Func {
    let params = if tail {
        vec![Ty::I32, Ty::I32]
    } else {
        vec![Ty::I32]
    };
    let mut f = Func::new(params, vec![Ty::I32]);
    let n = f.params[0];
    let [zero, n1, r, r1] = [(); 4].map(|_| f.new_var(Ty::I32));
    let recurse = if tail {
        let acc = f.params[1];
        prim(
            n1,
            Prim::Sub,
            vec![v(n), int(1)],
            prim(
                r1,
                Prim::Add,
                vec![v(acc), int(1)],
                let_(
                    vec![r],
                    Op::Call(MAIN, vec![], vec![]),
                    vec![v(n1), v(r1)],
                    ret(vec![v(r)]),
                ),
            ),
        )
    } else {
        prim(
            n1,
            Prim::Sub,
            vec![v(n), int(1)],
            let_(
                vec![r],
                Op::Call(MAIN, vec![], vec![]),
                vec![v(n1)],
                prim(r1, Prim::Add, vec![v(r), int(1)], ret(vec![v(r1)])),
            ),
        )
    };
    let base = if tail {
        ret(vec![v(f.params[1])])
    } else {
        ret(vec![int(0)])
    };
    f.body = prim(zero, Prim::Eq, vec![v(n), int(0)], if_(zero, base, recurse));
    f
}

#[test]
fn recursion() {
    assert_eq!(
        run_main(count(false), vec![Value::I32(1000)]),
        Ok(vec![Value::I32(1000)])
    );
}

#[test]
fn deep_recursion_overflows_the_stack() {
    let result = run_main(count(false), vec![Value::I32(1_000_000)]);
    assert_eq!(result.unwrap_err().kind, ErrorKind::StackOverflow);
}

#[test]
fn tail_calls_dont_grow_the_stack() {
    let result = run_main(count(true), vec![Value::I32(1_000_000), Value::I32(0)]);
    assert_eq!(result, Ok(vec![Value::I32(1_000_000)]));
}

#[test]
fn arithmetic() {
    let cases = [
        (Prim::Add, i32::MAX, 1, Ok(i32::MIN)),
        (Prim::Div(Sign::Signed), -7, 2, Ok(-3)),
        (Prim::Div(Sign::Unsigned), -1, 2, Ok(i32::MAX)),
        (Prim::Rem(Sign::Signed), i32::MIN, -1, Ok(0)),
        (Prim::Shl, 1, 33, Ok(2)),
        (Prim::Lt(Sign::Unsigned), -1, 0, Ok(0)),
        (Prim::Div(Sign::Signed), 7, 0, Ok(0)),
        (Prim::Div(Sign::Unsigned), -7, 0, Ok(0)),
        (Prim::Rem(Sign::Signed), -7, 0, Ok(-7)),
        (Prim::Rem(Sign::Unsigned), 7, 0, Ok(7)),
        (
            Prim::Div(Sign::Signed),
            i32::MIN,
            -1,
            Err(ErrorKind::IntegerOverflow),
        ),
    ];
    for (p, x, y, expected) in cases {
        let mut f = Func::new(vec![], vec![Ty::I32]);
        let r = f.new_var(Ty::I32);
        f.body = prim(r, p.clone(), vec![int(x), int(y)], ret(vec![v(r)]));
        let result = run_main(f, vec![]).map(|r| r[0]).map_err(|e| e.kind);
        assert_eq!(result, expected.map(Value::I32), "{p:?}({x}, {y})");
    }
}

#[test]
fn conversions() {
    let mut f = Func::new(vec![], vec![Ty::I64, Ty::F64]);
    let (wide, float) = (f.new_var(Ty::I64), f.new_var(Ty::F64));
    f.body = let_(
        vec![wide],
        Op::Prim(Prim::Convert(Ty::I64, Sign::Unsigned)),
        vec![int(-1)],
        let_(
            vec![float],
            Op::Prim(Prim::Convert(Ty::F64, Sign::Signed)),
            vec![int(-3)],
            ret(vec![v(wide), v(float)]),
        ),
    );
    assert_eq!(
        run_main(f, vec![]),
        Ok(vec![Value::I64(u32::MAX as i64), Value::F64(-3.0)])
    );
}

/// `sum_list(xs) = case xs { Nil => 0, Cons => xs.0 + sum_list(xs.1) }`
fn sum_list() -> Func {
    let list = Ty::data(LIST, Imm);
    let mut f = Func::new(vec![list.clone()], vec![Ty::I32]);
    let xs = f.params[0];
    let [head, rest_sum, total] = [(); 3].map(|_| f.new_var(Ty::I32));
    let tail = f.new_var(list);
    let cons = let_(
        vec![head],
        Op::Get(ctor(LIST, 1), 0),
        vec![v(xs)],
        let_(
            vec![tail],
            Op::Get(ctor(LIST, 1), 1),
            vec![v(xs)],
            let_(
                vec![rest_sum],
                Op::Call(G, vec![], vec![]),
                vec![v(tail)],
                prim(
                    total,
                    Prim::Add,
                    vec![v(head), v(rest_sum)],
                    ret(vec![v(total)]),
                ),
            ),
        ),
    );
    f.body = Block::from(Tail::Case {
        scrut: v(xs),
        arms: vec![
            Arm {
                tag: Tag::Ctor(ctor(LIST, 0)),
                body: ret(vec![int(0)]),
            },
            Arm {
                tag: Tag::Ctor(ctor(LIST, 1)),
                body: cons,
            },
        ],
        default: None,
    });
    f
}

#[test]
fn data_and_case() {
    // MAIN builds [1, 2, 3] and sums it with G.
    let list = Ty::data(LIST, Imm);
    let mut main = Func::new(vec![], vec![Ty::I32]);
    let [nil, l3, l2, l1] = [(); 4].map(|_| main.new_var(list.clone()));
    let r = main.new_var(Ty::I32);
    let cons = |bind, x, rest, body| {
        let_(
            vec![bind],
            Op::New(ctor(LIST, 1)),
            vec![int(x), v(rest)],
            body,
        )
    };
    main.body = let_(
        vec![nil],
        Op::New(ctor(LIST, 0)),
        vec![],
        cons(
            l3,
            3,
            nil,
            cons(
                l2,
                2,
                l3,
                cons(
                    l1,
                    1,
                    l2,
                    let_(
                        vec![r],
                        Op::Call(G, vec![], vec![]),
                        vec![v(l1)],
                        ret(vec![v(r)]),
                    ),
                ),
            ),
        ),
    );
    let mut m = module();
    m.funcs.insert(MAIN, main);
    m.funcs.insert(G, sum_list());
    assert_eq!(run(&m, vec![]), Ok(vec![Value::I32(6)]));
}

#[test]
fn closures_and_refs() {
    // G(counter: ref i32, by: i32) adds `by` to the counter.
    // MAIN makes a closure of G that captures a ref, applies it twice, and
    // reads the ref.
    let counter_ty = Ty::Ref(Box::new(Ty::I32));
    let mut g = Func::new(vec![counter_ty.clone(), Ty::I32], vec![]);
    let (counter, by) = (g.params[0], g.params[1]);
    let [old, new] = [(); 2].map(|_| g.new_var(Ty::I32));
    g.body = let_(
        vec![old],
        Op::RefGet,
        vec![v(counter)],
        prim(
            new,
            Prim::Add,
            vec![v(old), v(by)],
            let_(vec![], Op::RefSet, vec![v(counter), v(new)], ret(vec![])),
        ),
    );

    let closure_ty = Ty::Fn(
        Box::new(Sig {
            row: Row::NOTHING,
            params: vec![Ty::I32],
            results: vec![],
        }),
        Mut,
    );
    let mut main = Func::new(vec![], vec![Ty::I32]);
    let counter = main.new_var(counter_ty);
    let add = main.new_var(closure_ty);
    let total = main.new_var(Ty::I32);
    main.body = let_(
        vec![counter],
        Op::RefNew,
        vec![int(0)],
        let_(
            vec![add],
            Op::Closure(G, vec![], vec![]),
            vec![v(counter)],
            let_(
                vec![],
                Op::Apply,
                vec![v(add), int(5)],
                let_(
                    vec![],
                    Op::Apply,
                    vec![v(add), int(7)],
                    let_(
                        vec![total],
                        Op::RefGet,
                        vec![v(counter)],
                        ret(vec![v(total)]),
                    ),
                ),
            ),
        ),
    );
    let mut m = module();
    m.funcs.insert(MAIN, main);
    m.funcs.insert(G, g);
    assert_eq!(run(&m, vec![]), Ok(vec![Value::I32(12)]));
}

#[test]
fn arrays() {
    // xs = mut [0; 3]; xs[1] = 9; xs[1] + len(xs), then xs[3].
    let array = Ty::Array(Box::new(Ty::I32), Mut);
    let mut f = Func::new(vec![], vec![Ty::I32]);
    let xs = f.new_var(array);
    let [x, len, r, oob] = [(); 4].map(|_| f.new_var(Ty::I32));
    f.body = let_(
        vec![xs],
        Op::ArrayFill,
        vec![int(3), int(0)],
        let_(
            vec![],
            Op::ArraySet,
            vec![v(xs), int(1), int(9)],
            let_(
                vec![x],
                Op::ArrayGet,
                vec![v(xs), int(1)],
                let_(
                    vec![len],
                    Op::ArrayLen,
                    vec![v(xs)],
                    prim(r, Prim::Add, vec![v(x), v(len)], {
                        let_(
                            vec![oob],
                            Op::ArrayGet,
                            vec![v(xs), int(3)],
                            ret(vec![v(r)]),
                        )
                    }),
                ),
            ),
        ),
    );
    let result = run_main(f, vec![]);
    assert_eq!(
        result.unwrap_err().kind,
        ErrorKind::OutOfBounds { index: 3, len: 3 }
    );
}

#[test]
fn write_to_imm_object_is_an_error() {
    // Verified IR can't do this, so build it by hand: a Set on a point
    // that was created imm, through a variable typed mut.
    let mut f = Func::new(vec![], vec![]);
    let p = f.new_var(Ty::data(POINT, Imm));
    f.body = let_(
        vec![p],
        Op::New(ctor(POINT, 0)),
        vec![int(1), int(2)],
        let_(
            vec![],
            Op::Set(ctor(POINT, 0), 0),
            vec![v(p), int(3)],
            ret(vec![]),
        ),
    );
    let mut m = module();
    m.funcs.insert(MAIN, f);
    let result = Machine::new(&m, NoHost).call(MAIN, vec![]);
    assert_eq!(result.unwrap_err().kind, ErrorKind::WriteToImm);
}

/// `ArrayFreeze` makes the array itself `Imm`, so a wrong freeze shows up as a
/// write to it through an old alias.
#[test]
fn write_after_freeze_is_an_error() {
    let mut f = Func::new(vec![], vec![]);
    let xs = f.new_var(Ty::Array(Box::new(Ty::I32), Mut));
    let frozen = f.new_var(Ty::Array(Box::new(Ty::I32), Imm));
    f.body = let_(
        vec![xs],
        Op::ArrayFill,
        vec![int(2), int(0)],
        let_(
            vec![frozen],
            Op::ArrayFreeze,
            vec![v(xs)],
            let_(
                vec![],
                Op::ArraySet,
                vec![v(xs), int(0), int(3)],
                ret(vec![]),
            ),
        ),
    );
    let result = run_main(f, vec![]);
    assert_eq!(result.unwrap_err().kind, ErrorKind::WriteToImm);
}

fn box_unbox(at: Ty) -> Result<Vec<Value>, Error> {
    let mut f = Func::new(vec![], vec![at.clone()]);
    let boxed = f.new_var(Ty::Any(Imm));
    let x = f.new_var(at.clone());
    f.body = let_(
        vec![boxed],
        Op::Box(Ty::I32),
        vec![int(7)],
        let_(vec![x], Op::Unbox(at), vec![v(boxed)], ret(vec![v(x)])),
    );
    run_main(f, vec![])
}

#[test]
fn unbox_at_the_boxed_type() {
    assert_eq!(box_unbox(Ty::I32), Ok(vec![Value::I32(7)]));
}

#[test]
fn unbox_at_another_type_is_undefined() {
    let error = box_unbox(Ty::I64).unwrap_err();
    assert_eq!(
        error.kind,
        ErrorKind::Undefined("unbox of a i32 as a i64".into())
    );
}

#[test]
fn fuel_stops_an_infinite_loop() {
    let mut f = Func::new(vec![], vec![]);
    let lp = f.new_label();
    f.body = Block::from(Tail::Join {
        rec: true,
        defs: vec![JoinDef {
            label: lp,
            params: vec![],
            body: jump(lp, vec![]),
        }],
        body: Box::new(jump(lp, vec![])),
    });
    let mut m = module();
    m.funcs.insert(MAIN, f);
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    machine.fuel = Some(1000);
    assert_eq!(
        machine.call(MAIN, vec![]).unwrap_err().kind,
        ErrorKind::OutOfFuel
    );
}

#[test]
fn host_functions() {
    struct Doubler;
    impl Host for Doubler {
        fn call(&mut self, _: ExternId, _: &Extern, args: &[Value]) -> Result<Vec<Value>, String> {
            match args {
                [Value::I32(x)] => Ok(vec![Value::I32(x * 2)]),
                _ => Err("bad arguments".into()),
            }
        }
    }
    let mut m = module();
    let sig = Sig {
        row: Row::NOTHING,
        params: vec![Ty::I32],
        results: vec![Ty::I32],
    };
    m.externs.insert(
        ExternId(1),
        Extern {
            debug_name: None,
            module: "env".into(),
            name: "double".into(),
            sig,
        },
    );
    let mut f = Func::new(vec![], vec![Ty::I32]);
    let r = f.new_var(Ty::I32);
    f.body = let_(
        vec![r],
        Op::CallExtern(ExternId(1)),
        vec![int(21)],
        ret(vec![v(r)]),
    );
    m.funcs.insert(MAIN, f);
    verify_module(&m).unwrap();
    assert_eq!(
        Machine::new(&m, Doubler).call(MAIN, vec![]),
        Ok(vec![Value::I32(42)])
    );
    // `NoHost` has no functions, so the call fails and says which.
    let error = Machine::new(&m, NoHost).call(MAIN, vec![]).unwrap_err();
    assert_eq!(
        error.kind,
        ErrorKind::Host("no host function env.double".into())
    );
    assert_eq!(
        error.to_string(),
        "fn1: host function failed: no host function env.double"
    );
}

const YIELD: EffectId = EffectId(20);
const SUM: HandlerId = HandlerId(30);
const CLAUSE: FuncId = FuncId(4);
const RET_CLAUSE: FuncId = FuncId(5);

fn unit_sig() -> Sig {
    Sig {
        row: Row::NOTHING,
        params: vec![],
        results: vec![],
    }
}

/// The generator from the design doc: `G` yields 1 to `n`, and the handler
/// `SUM`, whose environment is a ref, adds what's yielded to it.
/// `clause(acc, x, k)` is built by `clause`.
fn generator(n: i32, clause: impl FnOnce(&mut Func, Var, Var, Var) -> Block) -> Module {
    let mut m = module();
    m.effects.insert(
        YIELD,
        EffectDecl {
            debug_name: None,
            ops: vec![Sig {
                row: Row::NOTHING,
                params: vec![Ty::I32],
                results: vec![],
            }],
        },
    );
    let acc_ty = Ty::Ref(Box::new(Ty::I32));
    let k_ty = Ty::Cont(Box::new(unit_sig()));

    let mut c = Func::new(vec![acc_ty.clone(), Ty::I32, k_ty], vec![]);
    let (acc, x, k) = (c.params[0], c.params[1], c.params[2]);
    c.body = clause(&mut c, acc, x, k);
    m.funcs.insert(CLAUSE, c);

    let mut r = Func::new(vec![acc_ty.clone()], vec![]);
    r.body = ret(vec![]);
    m.funcs.insert(RET_CLAUSE, r);
    m.handlers.insert(
        SUM,
        HandlerDecl {
            debug_name: None,
            effect: YIELD,
            effect_params: 0,
            env: vec![acc_ty.clone()],
            ops: vec![CLAUSE],
            ret: RET_CLAUSE,
            row: Row::NOTHING,
        },
    );

    // G: join rec loop(i) = if i > n { ret() } else { perform yield(i); jump loop(i + 1) }
    let mut g = Func::new(vec![], vec![]);
    g.row = Row::one(YIELD);
    let lp = g.new_label();
    let [i, done, i2] = [(); 3].map(|_| g.new_var(Ty::I32));
    let step = let_(
        vec![],
        Op::Perform(YIELD, 0),
        vec![v(i)],
        prim(i2, Prim::Add, vec![v(i), int(1)], jump(lp, vec![v(i2)])),
    );
    let body = prim(
        done,
        Prim::Gt(Sign::Signed),
        vec![v(i), int(n)],
        if_(done, ret(vec![]), step),
    );
    g.body = Block::from(Tail::Join {
        rec: true,
        defs: vec![JoinDef {
            label: lp,
            params: vec![i],
            body,
        }],
        body: Box::new(jump(lp, vec![int(1)])),
    });
    m.funcs.insert(G, g);

    // MAIN: acc = ref 0; handle SUM(acc, closure G()); ret(acc)
    let mut main = Func::new(vec![], vec![Ty::I32]);
    let acc = main.new_var(acc_ty);
    let body_sig = Sig {
        row: Row::one(YIELD),
        ..unit_sig()
    };
    let body = main.new_var(Ty::Fn(Box::new(body_sig), Imm));
    let total = main.new_var(Ty::I32);
    main.body = let_(
        vec![acc],
        Op::RefNew,
        vec![int(0)],
        let_(
            vec![body],
            Op::Closure(G, vec![], vec![]),
            vec![],
            let_(
                vec![],
                Op::Handle(SUM, vec![]),
                vec![v(acc), v(body)],
                let_(vec![total], Op::RefGet, vec![v(acc)], ret(vec![v(total)])),
            ),
        ),
    );
    m.funcs.insert(MAIN, main);
    m
}

/// The sum clause: `acc += x; resume(k)`, as a tail resume.
fn add_and_resume(f: &mut Func, acc: Var, x: Var, k: Var) -> Block {
    let [old, new] = [(); 2].map(|_| f.new_var(Ty::I32));
    let_(
        vec![old],
        Op::RefGet,
        vec![v(acc)],
        prim(
            new,
            Prim::Add,
            vec![v(old), v(x)],
            let_(
                vec![],
                Op::RefSet,
                vec![v(acc), v(new)],
                let_(vec![], Op::Resume, vec![v(k)], ret(vec![])),
            ),
        ),
    )
}

#[test]
fn handler_sums_a_generator() {
    let m = generator(4, add_and_resume);
    assert_eq!(run(&m, vec![]), Ok(vec![Value::I32(10)]));
}

#[test]
fn tail_resumes_dont_grow_the_stack() {
    let m = generator(100_000, add_and_resume);
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    machine.max_frames = 10;
    assert_eq!(
        machine.call(MAIN, vec![]),
        Ok(vec![Value::I32(705_082_704)])
    ); // wrapped
}

#[test]
fn clause_that_doesnt_resume_aborts() {
    // The clause adds x and returns without resuming: G stops at its first
    // yield.
    let m = generator(4, |f, acc, x, k| {
        let _ = k;
        let [old, new] = [(); 2].map(|_| f.new_var(Ty::I32));
        let_(
            vec![old],
            Op::RefGet,
            vec![v(acc)],
            prim(
                new,
                Prim::Add,
                vec![v(old), v(x)],
                let_(vec![], Op::RefSet, vec![v(acc), v(new)], ret(vec![])),
            ),
        )
    });
    assert_eq!(run(&m, vec![]), Ok(vec![Value::I32(1)]));
}

#[test]
fn resuming_twice_is_an_error() {
    // The clause captures k in a closure of H, which resumes it, and applies
    // the closure twice. The verifier can't see this; the machine can.
    let mut m = generator(4, |f, _, _, k| {
        let c = f.new_var(Ty::Fn(Box::new(unit_sig()), Mut));
        let_(
            vec![c],
            Op::Closure(H, vec![], vec![]),
            vec![v(k)],
            let_(
                vec![],
                Op::Apply,
                vec![v(c)],
                let_(vec![], Op::Apply, vec![v(c)], ret(vec![])),
            ),
        )
    });
    let mut h = Func::new(vec![Ty::Cont(Box::new(unit_sig()))], vec![]);
    let k = h.params[0];
    h.body = let_(vec![], Op::Resume, vec![v(k)], ret(vec![]));
    m.funcs.insert(H, h);
    let error = run(&m, vec![]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ContinuationResumedTwice);
}

#[test]
fn perform_without_a_handler_is_an_error() {
    let mut m = generator(4, add_and_resume);
    // Call G directly, without the handler: an entry point with effects,
    // which only the run time can catch.
    let mut main = Func::new(vec![], vec![Ty::I32]);
    main.row = Row::one(YIELD);
    main.body = let_(
        vec![],
        Op::Call(G, vec![], vec![]),
        vec![],
        ret(vec![int(0)]),
    );
    m.funcs.insert(MAIN, main);
    assert_eq!(
        run(&m, vec![]).unwrap_err().kind,
        ErrorKind::Unhandled(YIELD)
    );
}

#[test]
fn errors_say_where_they_happened() {
    let mut f = Func::new(vec![], vec![Ty::I32]);
    let r = f.new_var(Ty::I32);
    f.body = (ret(vec![v(r)])).with_let(Let {
        binds: vec![r],
        op: Op::Prim(Prim::Div(Sign::Signed)),
        args: vec![int(i32::MIN), int(-1)],
        source: Some(SourceId(42)),
    });
    let error = run_main(f, vec![]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "fn1 (src42): integer overflow in division"
    );
}

/// An element `array_alloc` made can be read once it's set, but reading
/// one before is undefined, also after `array_copy` copied it.
#[test]
fn unset_elements() {
    let m = conduit_ir::text::parse(
        "fn fn0(%0: i32) -> (i32) {
  let %1: mut [i32] = array_alloc(2i32)
  case %0 {
    1 =>
      let = array_set(%1, 1i32, 4i32)
      let %2: i32 = array_get(%1, 1i32)
      jump ret(%2)
    2 =>
      let %3: mut [i32] = array_alloc(2i32)
      let = array_copy(%3, 0i32, %1, 0i32, 2i32)
      let %4: i32 = array_get(%3, 1i32)
      jump ret(%4)
    _ =>
      let %5: i32 = array_get(%1, 1i32)
      jump ret(%5)
  }
}",
    )
    .unwrap();
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    assert_eq!(
        machine.call(FuncId(0), vec![Value::I32(1)]),
        Ok(vec![Value::I32(4)])
    );
    for which in [0, 2] {
        let error = machine
            .call(FuncId(0), vec![Value::I32(which)])
            .unwrap_err();
        assert_eq!(
            error.kind,
            ErrorKind::Undefined("array get of element 1, which isn't set".into())
        );
    }
}

/// What each kind of error says.
#[test]
fn error_messages() {
    let kinds = [
        (ErrorKind::IntegerOverflow, "integer overflow in division"),
        (
            ErrorKind::InvalidConversion,
            "a float converted to an integer is NaN or out of range",
        ),
        (
            ErrorKind::OutOfBounds { index: 3, len: 2 },
            "index 3 is out of bounds for length 2",
        ),
        (ErrorKind::Trap, "trap"),
        (
            ErrorKind::ContinuationResumedTwice,
            "a continuation was resumed more than once",
        ),
        (ErrorKind::Unhandled(EffectId(7)), "no handler for effect7"),
        (ErrorKind::StackOverflow, "stack overflow"),
        (ErrorKind::OutOfFuel, "out of fuel"),
        (ErrorKind::WriteToImm, "write to an imm object"),
        (ErrorKind::Undefined("x".into()), "undefined behavior: x"),
        (ErrorKind::Host("x".into()), "host function failed: x"),
        (ErrorKind::Unverified("x".into()), "invalid IR: x"),
    ];
    for (kind, message) in kinds {
        let error = Error {
            kind,
            func: None,
            source: None,
        };
        assert_eq!(error.to_string(), message);
    }
}

/// A function with a mutability parameter makes what its caller asks for:
/// a write to the `mut` array works, and one to the `imm` array is an
/// error.
#[test]
fn mutability_arguments_reach_the_callee() {
    let m = conduit_ir::text::parse(
        "fn fn0['m1]() -> ('m1 [i32]) {
  let %0: 'm1 [i32] = array_new(1i32)
  let = call fn1(%0)
  jump ret(%0)
}

fn fn1(%0: ro [i32]) -> () {
  jump ret()
}",
    )
    .unwrap();
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    let [Value::Obj(mut_array)] = machine.call_at(FuncId(0), vec![Mut], vec![]).unwrap()[..] else {
        panic!("expected an array");
    };
    let [Value::Obj(imm_array)] = machine.call_at(FuncId(0), vec![Imm], vec![]).unwrap()[..] else {
        panic!("expected an array");
    };
    let is_mut = |machine: &Machine<'_, Module, NoHost>, id| match machine.object(id) {
        Some(Object::Array { is_mut, .. }) => *is_mut,
        other => panic!("expected an array, got {other:?}"),
    };
    assert!(is_mut(&machine, mut_array));
    assert!(!is_mut(&machine, imm_array));

    let error = machine.call(FuncId(0), vec![]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid IR: fn0 takes 1 mutability arguments, but gets 0"
    );
    let error = machine
        .call_at(FuncId(0), vec![Mutability::ReadOnly], vec![])
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid IR: a mutability argument is imm or mut, not ro"
    );
}

/// What `undefined` gives can be passed on, but using it is undefined.
#[test]
fn using_undefined() {
    let m = conduit_ir::text::parse(
        "fn fn0(%0: i32) -> (i32) {
  let %1: i32 = undefined()
  case %0 {
    0 => jump ret(%1)
    _ =>
      let %2: i32 = add(%1, 1i32)
      jump ret(%2)
  }
}",
    )
    .unwrap();
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    let error = machine.call(FuncId(0), vec![Value::I32(1)]).unwrap_err();
    assert_eq!(
        error.kind,
        ErrorKind::Undefined("use of an undefined value".into())
    );
    assert!(machine.call(FuncId(0), vec![Value::I32(0)]).is_ok());
}
