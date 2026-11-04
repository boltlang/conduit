use super::*;

use Mutability::{Imm, Mut, ReadOnly};

const F: FuncId = FuncId(1);
const G: FuncId = FuncId(2);

/// `data Point = Point(i32, i32)`
const POINT: DataId = DataId(10);
/// `data Option = None | Some(i32)`
const OPTION: DataId = DataId(11);
/// `data Holder = Holder(mut Point)`: a field with its own mutability
const HOLDER: DataId = DataId(12);

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

fn module() -> Module {
    let mut m = Module::default();
    m.data.insert(
        POINT,
        DataDecl {
            params: 0,
            ctors: vec![CtorDecl {
                fields: vec![Ty::I32, Ty::I32],
            }],
        },
    );
    m.data.insert(
        OPTION,
        DataDecl {
            params: 0,
            ctors: vec![
                CtorDecl { fields: vec![] },
                CtorDecl {
                    fields: vec![Ty::I32],
                },
            ],
        },
    );
    m.data.insert(
        HOLDER,
        DataDecl {
            params: 0,
            ctors: vec![CtorDecl {
                fields: vec![Ty::data(POINT, Mut)],
            }],
        },
    );
    m
}

fn ctor(data: DataId, index: u32) -> CtorId {
    CtorId { data, index }
}

/// Verifies `f` as function `F` in [`module`], and gives the messages.
fn check(f: Func) -> Vec<String> {
    check_in(module(), f)
}

fn check_in(mut m: Module, f: Func) -> Vec<String> {
    m.funcs.insert(F, f);
    match verify_module(&m) {
        Ok(()) => vec![],
        Err(errors) => errors.into_iter().map(|e| e.message).collect(),
    }
}

fn assert_ok(f: Func) {
    assert_eq!(check(f), Vec::<String>::new());
}

fn assert_err(f: Func, expected: &str) {
    let errors = check(f);
    assert!(
        errors.iter().any(|e| e.contains(expected)),
        "expected an error containing {expected:?}, got {errors:?}"
    );
}

/// `sum` from the design doc.
fn sum() -> Func {
    let mut f = Func::new(vec![Ty::I32], vec![Ty::I32]);
    let n = f.params[0];
    let lp = f.new_label();
    let [i, acc, done, i2, acc2] = [(); 5].map(|_| f.new_var(Ty::I32));
    let step = let_(
        vec![i2],
        Op::Prim(Prim::Add),
        vec![v(i), int(1)],
        let_(
            vec![acc2],
            Op::Prim(Prim::Add),
            vec![v(acc), v(i)],
            jump(lp, vec![v(i2), v(acc2)]),
        ),
    );
    let body = let_(
        vec![done],
        Op::Prim(Prim::Ge(Sign::Signed)),
        vec![v(i), v(n)],
        Block::from(Tail::Case {
            scrut: v(done),
            arms: vec![Arm {
                tag: Tag::Int(1),
                body: ret(vec![v(acc)]),
            }],
            default: Some(Box::new(step)),
        }),
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
fn sum_is_ok() {
    assert_ok(sum());
}

#[test]
fn return_type_must_match() {
    let mut f = Func::new(vec![], vec![Ty::I64]);
    f.body = ret(vec![int(1)]);
    assert_err(f, "jump to ret: value 0 has type i32, but should have i64");
}

#[test]
fn var_must_be_in_scope() {
    // join k(x) = ret(x) in ret(x): `x` is only in scope in k's body.
    let mut f = Func::new(vec![], vec![Ty::I32]);
    let k = f.new_label();
    let x = f.new_var(Ty::I32);
    f.body = Block::from(Tail::Join {
        rec: false,
        defs: vec![JoinDef {
            label: k,
            params: vec![x],
            body: ret(vec![v(x)]),
        }],
        body: Box::new(ret(vec![v(x)])),
    });
    assert_err(f, "%0 is used where it isn't in scope");
}

#[test]
fn var_is_bound_once() {
    let mut f = Func::new(vec![], vec![Ty::I32]);
    let x = f.new_var(Ty::I32);
    let add = |body| let_(vec![x], Op::Prim(Prim::Add), vec![int(1), int(2)], body);
    f.body = add(add(ret(vec![v(x)])));
    assert_err(f, "%0 is bound more than once");
}

#[test]
fn non_recursive_join_cant_jump_to_itself() {
    let mut f = Func::new(vec![], vec![]);
    let k = f.new_label();
    f.body = Block::from(Tail::Join {
        rec: false,
        defs: vec![JoinDef {
            label: k,
            params: vec![],
            body: jump(k, vec![]),
        }],
        body: Box::new(jump(k, vec![])),
    });
    assert_err(f, "jump to j1, which isn't in scope");
}

#[test]
fn label_scope_ends_with_the_join() {
    // case 0 { 0 => join k() = ret() in ret(), _ => jump k() }
    let mut f = Func::new(vec![], vec![]);
    let k = f.new_label();
    let join = Block::from(Tail::Join {
        rec: false,
        defs: vec![JoinDef {
            label: k,
            params: vec![],
            body: ret(vec![]),
        }],
        body: Box::new(ret(vec![])),
    });
    f.body = Block::from(Tail::Case {
        scrut: int(0),
        arms: vec![Arm {
            tag: Tag::Int(0),
            body: join,
        }],
        default: Some(Box::new(jump(k, vec![]))),
    });
    assert_err(f, "jump to j1, which isn't in scope");
}

#[test]
fn jump_arguments_must_match() {
    let mut f = Func::new(vec![], vec![]);
    let k = f.new_label();
    let x = f.new_var(Ty::I32);
    f.body = Block::from(Tail::Join {
        rec: false,
        defs: vec![JoinDef {
            label: k,
            params: vec![x],
            body: ret(vec![]),
        }],
        body: Box::new(jump(k, vec![])),
    });
    assert_err(f, "jump to j1: expected 1 values, but got 0");
}

#[test]
fn integer_case_needs_a_default() {
    let mut f = Func::new(vec![Ty::I32], vec![]);
    let n = f.params[0];
    f.body = Block::from(Tail::Case {
        scrut: v(n),
        arms: vec![Arm {
            tag: Tag::Int(0),
            body: ret(vec![]),
        }],
        default: None,
    });
    assert_err(f, "case on i32 has no default");
}

#[test]
fn data_case_must_cover_every_constructor() {
    let mut f = Func::new(vec![Ty::data(OPTION, Imm)], vec![]);
    let o = f.params[0];
    f.body = Block::from(Tail::Case {
        scrut: v(o),
        arms: vec![Arm {
            tag: Tag::Ctor(ctor(OPTION, 1)),
            body: ret(vec![]),
        }],
        default: None,
    });
    assert_err(f, "doesn't cover every constructor");
}

/// `unwrap_or` from the design doc, reading the field of `Some` in `body`'s
/// place.
fn unwrap_or(read_in_arm: bool) -> Func {
    let mut f = Func::new(vec![Ty::data(OPTION, Imm), Ty::I32], vec![Ty::I32]);
    let (o, d) = (f.params[0], f.params[1]);
    let x = f.new_var(Ty::I32);
    let read = let_(
        vec![x],
        Op::Get(ctor(OPTION, 1), 0),
        vec![v(o)],
        ret(vec![v(x)]),
    );
    let (some, none) = if read_in_arm {
        (read, ret(vec![v(d)]))
    } else {
        (ret(vec![v(d)]), read)
    };
    f.body = Block::from(Tail::Case {
        scrut: v(o),
        arms: vec![
            Arm {
                tag: Tag::Ctor(ctor(OPTION, 1)),
                body: some,
            },
            Arm {
                tag: Tag::Ctor(ctor(OPTION, 0)),
                body: none,
            },
        ],
        default: None,
    });
    f
}

#[test]
fn field_read_under_its_arm_is_ok() {
    assert_ok(unwrap_or(true));
}

#[test]
fn field_read_needs_a_known_constructor() {
    assert_err(
        unwrap_or(false),
        "%0 isn't known to be built by data11.1 here",
    );
}

#[test]
fn single_constructor_fields_can_always_be_read() {
    let mut f = Func::new(vec![Ty::data(POINT, ReadOnly)], vec![Ty::I32]);
    let p = f.params[0];
    let x = f.new_var(Ty::I32);
    f.body = let_(
        vec![x],
        Op::Get(ctor(POINT, 0), 0),
        vec![v(p)],
        ret(vec![v(x)]),
    );
    assert_ok(f);
}

#[test]
fn only_mut_objects_can_be_set() {
    for (m, ok) in [(Mut, true), (Imm, false), (ReadOnly, false)] {
        let mut f = Func::new(vec![Ty::data(POINT, m)], vec![]);
        let p = f.params[0];
        f.body = let_(
            vec![],
            Op::Set(ctor(POINT, 0), 1),
            vec![v(p), int(5)],
            ret(vec![]),
        );
        if ok {
            assert_ok(f);
        } else {
            assert_err(f, "can't set a field of a");
        }
    }
}

#[test]
fn imm_object_cant_hold_a_mut_field() {
    let mut f = Func::new(vec![Ty::data(POINT, Mut)], vec![]);
    let p = f.params[0];
    let h = f.new_var(Ty::data(HOLDER, Imm));
    f.body = let_(vec![h], Op::New(ctor(HOLDER, 0)), vec![v(p)], ret(vec![]));
    assert_err(f, "an imm data12.0 can't hold field 0 of type mut data10");
}

#[test]
fn mut_object_can_hold_a_mut_field() {
    let mut f = Func::new(vec![Ty::data(POINT, Mut)], vec![]);
    let p = f.params[0];
    let h = f.new_var(Ty::data(HOLDER, Mut));
    f.body = let_(vec![h], Op::New(ctor(HOLDER, 0)), vec![v(p)], ret(vec![]));
    assert_ok(f);
}

#[test]
fn mut_and_imm_fit_read_only() {
    // G takes a read-only point. F passes it a mut one, then a read-only one
    // to a function that wants mut.
    let mut g = Func::new(vec![Ty::data(POINT, ReadOnly)], vec![]);
    g.body = ret(vec![]);
    let mut m = module();
    m.funcs.insert(G, g);

    let mut f = Func::new(vec![Ty::data(POINT, Mut)], vec![]);
    let p = f.params[0];
    f.body = let_(vec![], Op::Call(G, vec![]), vec![v(p)], ret(vec![]));
    assert_eq!(check_in(m.clone(), f), Vec::<String>::new());

    let mut h = Func::new(vec![Ty::data(POINT, Mut)], vec![]);
    h.body = ret(vec![]);
    m.funcs.insert(G, h);
    let mut f = Func::new(vec![Ty::data(POINT, ReadOnly)], vec![]);
    let p = f.params[0];
    f.body = let_(vec![], Op::Call(G, vec![]), vec![v(p)], ret(vec![]));
    let errors = check_in(m, f);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("value 0 has type ro data10, but should have mut data10")),
        "{errors:?}"
    );
}

/// F makes a closure of G, which captures a ref.
fn closure_capturing_ref(m: Mutability) -> Vec<String> {
    let mut g = Func::new(vec![Ty::Ref(Box::new(Ty::I32))], vec![]);
    g.body = ret(vec![]);
    let mut module = module();
    module.funcs.insert(G, g);

    let mut f = Func::new(vec![], vec![]);
    let r = f.new_var(Ty::Ref(Box::new(Ty::I32)));
    let c = f.new_var(Ty::Fn(
        Box::new(Sig {
            params: vec![],
            results: vec![],
        }),
        m,
    ));
    f.body = let_(
        vec![r],
        Op::RefNew,
        vec![int(0)],
        let_(vec![c], Op::Closure(G, vec![]), vec![v(r)], ret(vec![])),
    );
    check_in(module, f)
}

#[test]
fn imm_closure_cant_capture_a_ref() {
    let errors = closure_capturing_ref(Imm);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("an imm closure can't capture a ref i32")),
        "{errors:?}"
    );
    assert_eq!(closure_capturing_ref(Mut), Vec::<String>::new());
}

fn array(elem: Ty, m: Mutability) -> Ty {
    Ty::Array(Box::new(elem), m)
}

/// An `ArrayClone` shares the elements, so they must fit the new array.
#[test]
fn array_clone() {
    let mut f = Func::new(vec![array(Ty::data(POINT, Imm), Mut)], vec![]);
    let xs = f.params[0];
    let ys = f.new_var(array(Ty::data(POINT, Imm), Imm));
    f.body = let_(vec![ys], Op::ArrayClone, vec![v(xs)], ret(vec![]));
    assert_ok(f);

    // The points are mut, so an imm array can't share them.
    let mut f = Func::new(vec![array(Ty::data(POINT, Mut), Mut)], vec![]);
    let xs = f.params[0];
    let ys = f.new_var(array(Ty::data(POINT, Mut), Imm));
    f.body = let_(vec![ys], Op::ArrayClone, vec![v(xs)], ret(vec![]));
    assert_err(f, "an imm array can't hold elements of type mut data10");

    let mut f = Func::new(vec![array(Ty::data(POINT, Mut), Mut)], vec![]);
    let xs = f.params[0];
    let ys = f.new_var(array(Ty::data(POINT, Imm), Mut));
    f.body = let_(vec![ys], Op::ArrayClone, vec![v(xs)], ret(vec![]));
    assert_err(
        f,
        "array clone of a mut [mut data10] can't give a mut [imm data10]: its elements are mut data10",
    );

    let mut f = Func::new(vec![Ty::data(POINT, Mut)], vec![]);
    let p = f.params[0];
    let q = f.new_var(Ty::data(POINT, Imm));
    f.body = let_(vec![q], Op::ArrayClone, vec![v(p)], ret(vec![]));
    assert_err(f, "array clone needs one array");
}

/// `ArrayFreeze` has the types of an `ArrayClone` to `Imm`.
#[test]
fn array_freeze() {
    let mut f = Func::new(vec![array(Ty::I32, Mut)], vec![array(Ty::I32, Imm)]);
    let xs = f.params[0];
    let ys = f.new_var(array(Ty::I32, Imm));
    f.body = let_(vec![ys], Op::ArrayFreeze, vec![v(xs)], ret(vec![v(ys)]));
    assert_ok(f);

    let mut f = Func::new(vec![array(Ty::I32, Mut)], vec![]);
    let xs = f.params[0];
    let ys = f.new_var(array(Ty::I32, Mut));
    f.body = let_(vec![ys], Op::ArrayFreeze, vec![v(xs)], ret(vec![]));
    assert_err(f, "array freeze must bind one imm array");
}

#[test]
fn inherit_only_in_fields() {
    let mut f = Func::new(vec![Ty::data(POINT, Mutability::Inherit)], vec![]);
    f.body = ret(vec![]);
    assert_err(
        f,
        "`inherit` in inherit data10 is only allowed in field types",
    );
}

const YIELD: EffectId = EffectId(20);
const SUM: HandlerId = HandlerId(30);
const CLAUSE: FuncId = FuncId(3);
const RET_CLAUSE: FuncId = FuncId(4);

fn unit_sig() -> Sig {
    Sig {
        params: vec![],
        results: vec![],
    }
}

/// `effect Yield { yield(i32) -> () }` and a handler of it whose clause has
/// body `body`, with `k` its continuation.
fn handler_module(body: impl FnOnce(&mut Func, Var) -> Block) -> Module {
    let mut m = module();
    m.effects.insert(
        YIELD,
        EffectDecl {
            ops: vec![Sig {
                params: vec![Ty::I32],
                results: vec![],
            }],
        },
    );
    let mut clause = Func::new(vec![Ty::I32, Ty::Cont(Box::new(unit_sig()))], vec![]);
    let k = clause.params[1];
    clause.body = body(&mut clause, k);
    m.funcs.insert(CLAUSE, clause);
    let mut ret_clause = Func::new(vec![], vec![]);
    ret_clause.body = ret(vec![]);
    m.funcs.insert(RET_CLAUSE, ret_clause);
    m.handlers.insert(
        SUM,
        HandlerDecl {
            effect: YIELD,
            env: vec![],
            ops: vec![CLAUSE],
            ret: RET_CLAUSE,
        },
    );
    m
}

fn resume(k: Var, body: Block) -> Block {
    let_(vec![], Op::Resume, vec![v(k)], body)
}

fn verify(m: &Module) -> Vec<String> {
    match verify_module(m) {
        Ok(()) => vec![],
        Err(errors) => errors.into_iter().map(|e| e.message).collect(),
    }
}

#[test]
fn tail_resumptive_clause_is_ok() {
    let m = handler_module(|_, k| resume(k, ret(vec![])));
    assert_eq!(verify(&m), Vec::<String>::new());
}

#[test]
fn continuation_is_used_at_most_once() {
    let m = handler_module(|_, k| resume(k, resume(k, ret(vec![]))));
    let errors = verify(&m);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("continuation %1 may be used more than once")),
        "{errors:?}"
    );
}

#[test]
fn continuation_in_different_arms_is_used_once() {
    let m = handler_module(|_, k| {
        Block::from(Tail::Case {
            scrut: int(0),
            arms: vec![Arm {
                tag: Tag::Int(0),
                body: resume(k, ret(vec![])),
            }],
            default: Some(Box::new(resume(k, ret(vec![])))),
        })
    });
    assert_eq!(verify(&m), Vec::<String>::new());
}

#[test]
fn continuation_resumed_when_a_loop_ends_is_used_once() {
    // join rec loop() = case 0 { 0 => resume(k); ret(), _ => jump loop() }
    let m = handler_module(|f, k| {
        let lp = f.new_label();
        let body = Block::from(Tail::Case {
            scrut: int(0),
            arms: vec![Arm {
                tag: Tag::Int(0),
                body: resume(k, ret(vec![])),
            }],
            default: Some(Box::new(jump(lp, vec![]))),
        });
        Block::from(Tail::Join {
            rec: true,
            defs: vec![JoinDef {
                label: lp,
                params: vec![],
                body,
            }],
            body: Box::new(jump(lp, vec![])),
        })
    });
    assert_eq!(verify(&m), Vec::<String>::new());
}

#[test]
fn continuation_resumed_in_every_iteration_is_used_more_than_once() {
    // join rec loop() = resume(k); jump loop()
    let m = handler_module(|f, k| {
        let lp = f.new_label();
        Block::from(Tail::Join {
            rec: true,
            defs: vec![JoinDef {
                label: lp,
                params: vec![],
                body: resume(k, jump(lp, vec![])),
            }],
            body: Box::new(jump(lp, vec![])),
        })
    });
    let errors = verify(&m);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("may be used more than once")),
        "{errors:?}"
    );
}

#[test]
fn continuation_used_before_a_jump_and_in_the_join_point() {
    // join j() = resume(k); ret() in resume(k); jump j()
    let m = handler_module(|f, k| {
        let j = f.new_label();
        Block::from(Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: j,
                params: vec![],
                body: resume(k, ret(vec![])),
            }],
            body: Box::new(resume(k, jump(j, vec![]))),
        })
    });
    let errors = verify(&m);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("may be used more than once")),
        "{errors:?}"
    );
}

#[test]
fn handler_clause_must_take_the_continuation() {
    let mut m = handler_module(|_, _| ret(vec![]));
    let mut clause = Func::new(vec![Ty::I32], vec![]);
    clause.body = ret(vec![]);
    m.funcs.insert(CLAUSE, clause);
    let errors = verify(&m);
    assert!(
        errors.iter().any(|e| e.contains("clause fn3 for operation 0 has type (i32) -> (), but should have (i32, cont() -> ()) -> ()")),
        "{errors:?}"
    );
}

#[test]
fn handle_and_perform() {
    // F: let body = closure G(); handle SUM(body); ret()
    // G: perform YIELD.0(1); ret()
    let mut m = handler_module(|_, k| resume(k, ret(vec![])));
    let mut g = Func::new(vec![], vec![]);
    g.body = let_(vec![], Op::Perform(YIELD, 0), vec![int(1)], ret(vec![]));
    m.funcs.insert(G, g);

    let mut f = Func::new(vec![], vec![]);
    let body = f.new_var(Ty::Fn(Box::new(unit_sig()), Imm));
    f.body = let_(
        vec![body],
        Op::Closure(G, vec![]),
        vec![],
        let_(vec![], Op::Handle(SUM), vec![v(body)], ret(vec![])),
    );
    m.funcs.insert(F, f);
    assert_eq!(verify(&m), Vec::<String>::new());
}

#[test]
fn errors_name_their_function() {
    let mut f = Func::new(vec![], vec![Ty::I64]);
    f.body = ret(vec![int(1)]);
    let mut m = module();
    m.funcs.insert(F, f);
    let errors = verify_module(&m).unwrap_err();
    assert_eq!(errors[0].item, Item::Func(F));
    assert_eq!(
        errors[0].to_string(),
        "fn1: jump to ret: value 0 has type i32, but should have i64"
    );
}

#[test]
fn call_of_a_broken_function_is_an_error() {
    // G's parameter has no type. Checking F's call of G must not panic.
    let mut g = Func::new(vec![], vec![]);
    g.params.push(Var(5));
    g.body = ret(vec![]);
    let mut m = module();
    m.funcs.insert(G, g);
    let mut f = Func::new(vec![], vec![]);
    f.body = let_(vec![], Op::Call(G, vec![]), vec![], ret(vec![]));
    let errors = check_in(m, f);
    assert!(
        errors.iter().any(|e| e.contains("unknown function fn2")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("%5 has no type")),
        "{errors:?}"
    );
}

/// A closure of G, which resumes the continuation it captured.
fn closure_resuming(m: &mut Module, k_ty: Ty) {
    let mut g = Func::new(vec![k_ty], vec![]);
    let k = g.params[0];
    g.body = resume(k, ret(vec![]));
    m.funcs.insert(G, g);
}

#[test]
fn continuation_used_through_a_closure_is_left_to_run_time() {
    // The clause captures k in a closure and applies it twice. The
    // verifier only sees the capture, so it accepts this: resuming k the
    // second time traps at run time instead.
    let k_ty = Ty::Cont(Box::new(unit_sig()));
    let closure_ty = Ty::Fn(Box::new(unit_sig()), Mut);
    let mut m = handler_module(|f, k| {
        let c = f.new_var(closure_ty);
        let_(
            vec![c],
            Op::Closure(G, vec![]),
            vec![v(k)],
            let_(
                vec![],
                Op::Apply,
                vec![v(c)],
                let_(vec![], Op::Apply, vec![v(c)], ret(vec![])),
            ),
        )
    });
    closure_resuming(&mut m, k_ty);
    assert_eq!(verify(&m), Vec::<String>::new());
}

#[test]
fn continuation_captured_and_resumed_counts_twice() {
    let k_ty = Ty::Cont(Box::new(unit_sig()));
    let closure_ty = Ty::Fn(Box::new(unit_sig()), Mut);
    let mut m = handler_module(|f, k| {
        let c = f.new_var(closure_ty);
        let_(
            vec![c],
            Op::Closure(G, vec![]),
            vec![v(k)],
            resume(k, ret(vec![])),
        )
    });
    closure_resuming(&mut m, k_ty);
    let errors = verify(&m);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("may be used more than once")),
        "{errors:?}"
    );
}

#[test]
fn branches_on_the_same_condition_are_rejected() {
    // join rest() = case 0 { 0 => resume(k); ret(), _ => ret() }
    // in case 0 { 1 => resume(k); jump rest(), _ => jump rest() }
    //
    // Every run resumes k once, but the verifier doesn't know that both
    // cases test the same value, so it sees a path with two uses.
    let m = handler_module(|f, k| {
        let rest = f.new_label();
        let second = Block::from(Tail::Case {
            scrut: int(0),
            arms: vec![Arm {
                tag: Tag::Int(0),
                body: resume(k, ret(vec![])),
            }],
            default: Some(Box::new(ret(vec![]))),
        });
        let first = Block::from(Tail::Case {
            scrut: int(0),
            arms: vec![Arm {
                tag: Tag::Int(1),
                body: resume(k, jump(rest, vec![])),
            }],
            default: Some(Box::new(jump(rest, vec![]))),
        });
        Block::from(Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: rest,
                params: vec![],
                body: second,
            }],
            body: Box::new(first),
        })
    });
    let errors = verify(&m);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("may be used more than once")),
        "{errors:?}"
    );
}

#[test]
fn branches_through_a_join_point_are_counted_per_path() {
    // join j() = resume(k); ret() in case 0 { 0 => resume(k); ret(), _ => jump j() }
    let m = handler_module(|f, k| {
        let j = f.new_label();
        Block::from(Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: j,
                params: vec![],
                body: resume(k, ret(vec![])),
            }],
            body: Box::new(Block::from(Tail::Case {
                scrut: int(0),
                arms: vec![Arm {
                    tag: Tag::Int(0),
                    body: resume(k, ret(vec![])),
                }],
                default: Some(Box::new(jump(j, vec![]))),
            })),
        })
    });
    assert_eq!(verify(&m), Vec::<String>::new());
}

#[test]
fn long_functions_dont_need_a_deep_stack() {
    let mut f = Func::new(vec![Ty::I32], vec![Ty::I32]);
    let mut last = f.params[0];
    let mut lets = Vec::new();
    for _ in 0..200_000 {
        let next = f.new_var(Ty::I32);
        lets.push(Let {
            binds: vec![next],
            op: Op::Prim(Prim::Add),
            args: vec![v(last), int(1)],
            source: None,
        });
        last = next;
    }
    f.body = Block {
        lets,
        tail: Tail::Jump {
            target: Label::RET,
            args: vec![v(last)],
            source: None,
        },
    };
    assert_ok(f);
}
