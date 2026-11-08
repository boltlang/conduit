use conduit_ir::verify::verify_module;
use conduit_ir::*;

use crate::*;

use Mutability::{Imm, Mut};

const MAIN: FuncId = FuncId(1);
const G: FuncId = FuncId(2);

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
    // that was created imm, which the verifier would reject.
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
        fn call(
            &mut self,
            _: &str,
            _: &Extern,
            args: &[Value],
            _: &mut Arrays<'_, '_>,
        ) -> Result<Vec<Value>, HostError> {
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
    let link = Link::Emu {
        name: "double".into(),
    };
    m.externs
        .insert(ExternId(1), Extern::new(link, sig.clone()));
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
        ErrorKind::Host("no host function double".into())
    );
    assert_eq!(
        error.to_string(),
        "fn1: host function failed: no host function double"
    );
    // An extern for another target isn't the host's to call.
    let link = Link::Wasm {
        module: "env".into(),
        name: "double".into(),
    };
    m.externs.insert(ExternId(1), Extern::new(link, sig));
    let error = Machine::new(&m, Doubler).call(MAIN, vec![]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "fn1: host function failed: extern1 has a wasm link, for another target: \
         the evaluator calls emu ones"
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
            effect_params: 0,
            env: vec![acc_ty.clone()],
            ops: vec![(YIELD, vec![CLAUSE])],
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
        (ErrorKind::Exit(3), "exited with code 3"),
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
/// a `mut` array or an `imm` one. A call without the argument, or with
/// `ro`, is invalid IR.
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

/// Runs `fn0` of a module with counting on, and gives its results and
/// what it leaked, with the results as the roots.
fn counted(src: &str, args: Vec<Value>) -> Result<(Vec<Value>, Leaks), Error> {
    let m = conduit_ir::text::parse(src).unwrap();
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    machine.counting = true;
    let results = machine.call(FuncId(0), args)?;
    let leaks = machine.leaks(&results);
    Ok((results, leaks))
}

const LIST_TEXT: &str = "data List {
  ()
  (i32, inherit List)
}
";

/// `fn0` builds a list of 1..=n, which `fn2` sums, consuming it. `nil_drop`
/// is what the empty arm does with the list.
fn sum_list_text(nil_drop: &str) -> String {
    format!(
        "{LIST_TEXT}
fn fn0(%n: i32) -> (i32) {{
  let %l: imm List = call fn1(%n)
  let %r: i32 = call fn2(%l)
  jump ret(%r)
}}

fn fn1(%n: i32) -> (imm List) {{
  case %n {{
    0 =>
      let %nil: imm List = new List.0()
      jump ret(%nil)
    _ =>
      let %n1: i32 = sub(%n, 1i32)
      let %rest: imm List = call fn1(%n1)
      let %list: imm List = new List.1(%n, %rest)
      jump ret(%list)
  }}
}}

fn fn2(%l: imm List) -> (i32) {{
  case %l {{
    List.0 =>
      {nil_drop}
      jump ret(0i32)
    List.1 =>
      let %head: i32 = get List.1.0(%l)
      let %tail: imm List = get List.1.1(%l)
      let = dup(%tail)
      let = drop(%l)
      let %rest: i32 = call fn2(%tail)
      let %r: i32 = add(%head, %rest)
      jump ret(%r)
  }}
}}"
    )
}

/// A list consumed with a `drop` of every cell is freed whole.
#[test]
fn counted_list_is_freed() {
    let (results, leaks) = counted(&sum_list_text("let = drop(%l)"), vec![Value::I32(4)]).unwrap();
    assert_eq!(results, vec![Value::I32(10)]);
    assert!(leaks.is_empty(), "{leaks}");
}

/// Without the `drop` of the empty list, it's reported, with nothing else.
#[test]
fn missing_drop_is_reported() {
    let (_, leaks) = counted(&sum_list_text(""), vec![Value::I32(4)]).unwrap();
    assert_eq!(leaks.to_string(), "undropped: #0 (+1)");
}

/// A `drop` too many frees the empty list too early, and the next use of it
/// is undefined.
#[test]
fn use_after_free_is_undefined() {
    let error = counted(
        &sum_list_text("let = drop(%l)\n      let = drop(%l)"),
        vec![Value::I32(4)],
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "fn2: undefined behavior: drop of a freed object"
    );
    let src = format!(
        "{LIST_TEXT}
fn fn0() -> (i32) {{
  let %nil: imm List = new List.0()
  let %l: imm List = new List.1(1i32, %nil)
  let = drop(%l)
  let %x: i32 = get List.1.0(%l)
  jump ret(%x)
}}"
    );
    let error = counted(&src, vec![]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "fn0: undefined behavior: use of a freed object"
    );
}

/// Two fields holding one object that was counted once: the object is
/// dropped too often, and once the pair is freed, the root refers to a
/// freed object.
#[test]
fn wrong_counts_are_reported() {
    let src = format!(
        "{LIST_TEXT}
data Pair {{
  (imm List, imm List)
}}

fn fn0() -> (imm Pair, imm List) {{
  let %nil: imm List = new List.0()
  let %pair: imm Pair = new Pair.0(%nil, %nil)
  let %l: imm List = new List.0()
  let = drop(%l)
  jump ret(%pair, %l)
}}"
    );
    let (_, leaks) = counted(&src, vec![]).unwrap();
    assert_eq!(
        leaks.to_string(),
        "dropped too often: #0 (-1); freed but referred to: #2"
    );
}

/// A ref that holds a box of itself is a cycle: its counts are right, but
/// counting can't free it.
#[test]
fn cycles_are_reported_apart() {
    let src = "fn fn0() -> () {
  let %u: mut any = undefined()
  let %r: ref mut any = ref_new(%u)
  let = dup(%r)
  let %a: mut any = box[ref mut any](%r)
  let = ref_set(%r, %a)
  let = drop(%r)
  jump ret()
}";
    let (_, leaks) = counted(src, vec![]).unwrap();
    assert!(leaks.counts_ok(), "{leaks}");
    assert_eq!(leaks.cycles, vec![ObjId(0), ObjId(1)]);
    assert_eq!(leaks.to_string(), "kept alive by cycles: #0, #1");
}

/// Constants are never counted, and `dup` and `drop` of an undefined value
/// do nothing.
#[test]
fn constants_and_undefined_arent_counted() {
    let src = format!(
        "{LIST_TEXT}
const nil: imm List = List.0()

fn fn0() -> () {{
  let %c: imm List = const nil()
  let = drop(%c)
  let = drop(%c)
  let %u: imm List = undefined()
  let = dup(%u)
  let = drop(%u)
  jump ret()
}}"
    );
    let (_, leaks) = counted(&src, vec![]).unwrap();
    assert!(leaks.is_empty(), "{leaks}");
}

/// Without counting, `drop` does nothing, so dropping twice is fine.
#[test]
fn without_counting_nothing_is_freed() {
    let m =
        conduit_ir::text::parse(&sum_list_text("let = drop(%l)\n      let = drop(%l)")).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    assert_eq!(
        machine.call(FuncId(0), vec![Value::I32(3)]).unwrap(),
        vec![Value::I32(6)]
    );
    assert_eq!(machine.objects().count(), 4);
}

/// The ops that put references into objects or take them out count them:
/// `array_fill` holds its value once per element, `array_clone` shares its
/// elements, `array_copy` and `array_set` drop what they overwrite.
#[test]
fn array_ops_count_their_elements() {
    let src = format!(
        "{LIST_TEXT}
fn fn0() -> () {{
  let %nil: imm List = new List.0()
  let %xs: mut [imm List] = array_fill(3i32, %nil)
  let %ys: mut [imm List] = array_clone(%xs)
  let %one: imm List = new List.0()
  let = array_set(%ys, 0i32, %one)
  let = array_copy(%xs, 0i32, %ys, 0i32, 2i32)
  let = dup(%nil)
  let %none: mut [imm List] = array_fill(0i32, %nil)
  let = drop(%xs)
  let = drop(%ys)
  let = drop(%none)
  jump ret()
}}"
    );
    // The empty `array_fill` takes a reference to `%nil` and drops it.
    let (_, leaks) = counted(&src, vec![]).unwrap();
    assert!(leaks.is_empty(), "{leaks}");
}

/// `ref_set` and `set` drop the value they replace.
#[test]
fn writes_drop_what_they_replace() {
    let src = format!(
        "{LIST_TEXT}
data Box {{
  (imm List)
}}

fn fn0() -> () {{
  let %a: imm List = new List.0()
  let %r: ref imm List = ref_new(%a)
  let %b: imm List = new List.0()
  let = ref_set(%r, %b)
  let %c: imm List = new List.0()
  let %box: mut Box = new Box.0(%c)
  let %d: imm List = new List.0()
  let = set Box.0.0(%box, %d)
  let = drop(%r)
  let = drop(%box)
  jump ret()
}}"
    );
    let (_, leaks) = counted(&src, vec![]).unwrap();
    assert!(leaks.is_empty(), "{leaks}");
}

/// A handler's environment is the handler's, and each clause gets its own
/// reference; `handle` takes the closure, and `resume` the continuation. With every owner
/// dropping what it owns, a generator under a handler frees everything.
#[test]
fn handlers_count_their_environment() {
    let src = "effect Yield {
  (i32) -> ()
}

handler handler0 for Yield env(ref i32) ops(fn1) ret fn2

fn fn0() -> (i32) {
  let %acc: ref i32 = ref_new(0i32)
  let %body: imm fn() -> () does Yield = closure fn3()
  let = dup(%acc)
  let = handle handler0(%acc, %body)
  let %total: i32 = ref_get(%acc)
  let = drop(%acc)
  jump ret(%total)
}

fn fn1(%acc: ref i32, %x: i32, %k: cont() -> ()) -> () {
  let %old: i32 = ref_get(%acc)
  let %new: i32 = add(%old, %x)
  let = ref_set(%acc, %new)
  let = drop(%acc)
  let = resume(%k)
  jump ret()
}

fn fn2(%acc: ref i32) -> () {
  let = drop(%acc)
  jump ret()
}

fn fn3() -> () does Yield {
  let = perform Yield.0(1i32)
  let = perform Yield.0(2i32)
  jump ret()
}";
    let (results, leaks) = counted(src, vec![]).unwrap();
    assert_eq!(results, vec![Value::I32(3)]);
    assert!(leaks.is_empty(), "{leaks}");
}

/// `is_unique` tells a count of 1, `free` needs one and `decref` needs more.
#[test]
fn unique_free_and_decref() {
    let src = |shared: &str, op: &str| {
        format!(
            "{LIST_TEXT}
fn fn0() -> (i32) {{
  let %l: imm List = new List.0()
  {shared}
  let %u: i32 = is_unique(%l)
  let = {op}(%l)
  jump ret(%u)
}}"
        )
    };
    let (results, leaks) = counted(&src("", "free"), vec![]).unwrap();
    assert_eq!(results, vec![Value::I32(1)]);
    assert!(leaks.is_empty(), "{leaks}");
    let (results, leaks) = counted(&src("let = dup(%l)", "decref"), vec![]).unwrap();
    assert_eq!(results, vec![Value::I32(0)]);
    assert_eq!(leaks.to_string(), "undropped: #0 (+1)");
    let error = counted(&src("let = dup(%l)", "free"), vec![]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "fn0: undefined behavior: free of a shared object"
    );
    let error = counted(&src("", "decref"), vec![]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "fn0: undefined behavior: decref of a unique object"
    );
}

/// With cycles collected too, the cycle of a ref that holds a box of a
/// pair that holds a box of the ref is freed, all four; the list the pair
/// holds, which a root holds too, stays.
#[test]
fn cycles_are_collected() {
    let src = format!(
        "{LIST_TEXT}
data Pair {{
  (mut any, imm List)
}}

fn fn0() -> (imm List) {{
  let %u: mut any = undefined()
  let %r: ref mut any = ref_new(%u)
  let %nil: imm List = new List.0()
  let %kept: imm List = new List.1(1i32, %nil)
  let = dup(%kept)
  let = dup(%r)
  let %a: mut any = box[ref mut any](%r)
  let %p: mut Pair = new Pair.0(%a, %kept)
  let %b: mut any = box[mut Pair](%p)
  let = ref_set(%r, %b)
  let = drop(%r)
  jump ret(%kept)
}}"
    );
    let m = conduit_ir::text::parse(&src).unwrap();
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    machine.counting = true;
    machine.cycles = true;
    let results = machine.call(FuncId(0), vec![]).unwrap();
    assert_eq!(machine.leaks(&results).cycles.len(), 4);
    machine.collect_cycles();
    let leaks = machine.leaks(&results);
    assert!(leaks.is_empty(), "{leaks}");
    let Value::Obj(kept) = results[0] else {
        panic!()
    };
    assert_eq!(machine.count(kept), Some(Count::Live(1)));
}

/// The stats of a counted run: what it allocated and freed, the most live
/// at once, and the count ops it ran.
#[test]
fn stats_count_what_a_run_does() {
    let m = conduit_ir::text::parse(&sum_list_text("let = drop(%l)")).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    machine.counting = true;
    machine.call(FuncId(0), vec![Value::I32(3)]).unwrap();
    let stats = machine.stats;
    assert_eq!((stats.allocs, stats.frees, stats.peak_live), (4, 4, 4));
    // A drop per cell, and a dup per tail kept.
    assert_eq!(stats.count_ops, 4 + 3);
    assert!(stats.steps > 0);
    // As benchmarks show it: everything but the steps is known here.
    let shown = stats.to_string();
    assert!(
        shown.ends_with(
            ", allocs 4, frees 4, peak live 4, count ops 7, reused 0 of 0, collections 0"
        ),
        "{shown}"
    );
}

/// A host function can end the program with an exit code, which no
/// handler catches, and which ends the run whatever is below it.
#[test]
fn hosts_can_exit() {
    struct Exits;
    impl Host for Exits {
        fn call(
            &mut self,
            name: &str,
            _: &Extern,
            args: &[Value],
            _: &mut Arrays<'_, '_>,
        ) -> Result<Vec<Value>, HostError> {
            match (name, args) {
                ("exit", [Value::I32(code)]) => Err(HostError::Exit(*code)),
                _ => Err("no such function".into()),
            }
        }
    }
    let m = conduit_ir::text::parse(
        "effect Fail {
  () -> ()
}
#[link(emu)]
extern exit(i32) -> ()
handler caught for Fail env() ops(fail) ret done
fn fail(%k: cont() -> (i32)) -> (i32) {
  jump ret(-1i32)
}
fn done(%r: i32) -> (i32) {
  jump ret(%r)
}
fn body(%n: i32) -> (i32) does Fail {
  let = call_extern exit(%n)
  let = perform Fail.0()
  jump ret(0i32)
}
fn main(%n: i32) -> (i32) {
  let %b: imm fn() -> (i32) does Fail = closure body(%n)
  let %r: i32 = handle caught(%b)
  jump ret(%r)
}",
    )
    .unwrap();
    verify_module(&m).unwrap();
    let main = conduit_ir::text::find_func(&m, "main").unwrap();
    let error = Machine::new(&m, Exits)
        .call(main, vec![Value::I32(3)])
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Exit(3));
}

/// Every op that counts finds an object that's been freed, not only `drop`
/// and uses: each is undefined behavior, named after the op.
#[test]
fn counting_a_freed_object_is_undefined() {
    let ops = [
        ("dup", "let = dup(%l)"),
        ("drop_reuse", "let %t: reuse List.0 = drop_reuse(%l)"),
        ("is_unique", "let %u: i32 = is_unique(%l)"),
        ("free", "let = free(%l)"),
        ("decref", "let = decref(%l)"),
    ];
    for (op, line) in ops {
        let src = format!(
            "{LIST_TEXT}
fn fn0() -> () {{
  let %l: imm List = new List.0()
  let = drop(%l)
  {line}
  jump ret()
}}"
        );
        let error = counted(&src, vec![]).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("fn0: undefined behavior: {op} of a freed object")
        );
    }
}

/// A reuse token that no `reuse` takes holds memory that's never given
/// back, which is reported as undropped.
#[test]
fn unused_reuse_token_is_reported() {
    let src = format!(
        "{LIST_TEXT}
fn fn0() -> () {{
  let %l: imm List = new List.0()
  let %t: reuse List.0 = drop_reuse(%l)
  jump ret()
}}"
    );
    let (_, leaks) = counted(&src, vec![]).unwrap();
    assert_eq!(leaks.to_string(), "undropped: #0 (+1)");
}

/// Without counting, the ops that count do nothing: `drop_reuse` gives no
/// memory, so `reuse` allocates, and `free` and `decref` change nothing,
/// even of an object that's shared or unique.
#[test]
fn counting_ops_without_counting_do_nothing() {
    let src = format!(
        "{LIST_TEXT}
fn fn0() -> (i32) {{
  let %nil: imm List = new List.0()
  let %l: imm List = new List.1(7i32, %nil)
  let = decref(%l)
  let = free(%nil)
  let %t: reuse List.1 = drop_reuse(%l)
  let %l2: imm List = reuse List.1(%t, 8i32, %nil)
  let %x: i32 = get List.1.0(%l2)
  let %u: i32 = is_unique(%l2)
  let %r: i32 = add(%x, %u)
  jump ret(%r)
}}"
    );
    let m = conduit_ir::text::parse(&src).unwrap();
    verify_module(&m).unwrap();
    let mut machine = Machine::new(&m, NoHost);
    assert_eq!(
        machine.call(FuncId(0), vec![]).unwrap(),
        vec![Value::I32(8)]
    );
    assert_eq!(machine.stats.reused, 0);
    assert_eq!(machine.stats.fresh, 1);
}

/// What a continuation that's dropped without being resumed held is lost
/// with it: its frames aren't dropped, so the list the body was holding at
/// the `perform` is reported apart.
#[test]
fn what_a_dropped_continuation_held_is_reported() {
    let src = format!(
        "{LIST_TEXT}
effect Fail {{
  () -> ()
}}

handler stop for Fail env() ops(fail) ret done

fn fn0() -> (i32) {{
  let %b: imm fn() -> (i32) does Fail = closure body()
  let %r: i32 = handle stop(%b)
  jump ret(%r)
}}

fn fail(%k: cont() -> (i32)) -> (i32) {{
  let = drop(%k)
  jump ret(0i32)
}}

fn done(%r: i32) -> (i32) {{
  jump ret(%r)
}}

fn body() -> (i32) does Fail {{
  let %l: imm List = new List.0()
  let = perform Fail.0()
  let = drop(%l)
  jump ret(1i32)
}}"
    );
    let (results, leaks) = counted(&src, vec![]).unwrap();
    assert_eq!(results, vec![Value::I32(0)]);
    assert!(leaks.counts_ok(), "{leaks}");
    assert_eq!(leaks.to_string(), "lost with continuations: #1 (+1)");
}

/// A host function must give what its extern says: as many results, and
/// host values exactly where it has `host`.
#[test]
fn host_results_must_fit_the_extern() {
    struct Wrong;
    impl Host for Wrong {
        fn call(
            &mut self,
            name: &str,
            _: &Extern,
            _: &[Value],
            _: &mut Arrays<'_, '_>,
        ) -> Result<Vec<Value>, HostError> {
            Ok(match name {
                "two" => vec![Value::I32(1), Value::I32(2)],
                "host_for_i32" => vec![Value::Host(3)],
                _ => vec![Value::I32(4)],
            })
        }
    }
    let src = "#[link(emu)]
extern two() -> (i32)
#[link(emu)]
extern host_for_i32() -> (i32)
#[link(emu)]
extern i32_for_host() -> (host)

fn fn0() -> () {
  let %a: i32 = call_extern two()
  jump ret()
}

fn fn1() -> () {
  let %a: i32 = call_extern host_for_i32()
  jump ret()
}

fn fn2() -> () {
  let %a: host = call_extern i32_for_host()
  jump ret()
}";
    let m = conduit_ir::text::parse(src).unwrap();
    verify_module(&m).unwrap();
    let expected = [
        "two gave 2 results instead of 1",
        "host_for_i32 gave a host result where its extern has a i32",
        "i32_for_host gave a non-host result where its extern has a host",
    ];
    for (f, message) in expected.iter().enumerate() {
        let error = Machine::new(&m, Wrong)
            .call(FuncId(f as u64), vec![])
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
}
