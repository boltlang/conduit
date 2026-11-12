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
            debug_name: None,
            params: 0,
            effect_params: 0,
            ctors: vec![CtorDecl {
                fields: vec![Ty::I32, Ty::I32],
            }],
        },
    );
    m.data.insert(
        OPTION,
        DataDecl {
            debug_name: None,
            params: 0,
            effect_params: 0,
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
            debug_name: None,
            params: 0,
            effect_params: 0,
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
        row: Row::NOTHING,
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
            debug_name: None,
            ops: vec![Sig {
                row: Row::NOTHING,
                params: vec![Ty::I32],
                results: vec![],
            }],
            link: None,
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
            debug_name: None,
            effect_params: 0,
            env: vec![],
            ops: vec![(YIELD, vec![CLAUSE])],
            ret: RET_CLAUSE,
            row: Row::NOTHING,
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
fn handler_clause_must_take_the_continuation() {
    let mut m = handler_module(|_, _| ret(vec![]));
    let mut clause = Func::new(vec![Ty::I32], vec![]);
    clause.body = ret(vec![]);
    m.funcs.insert(CLAUSE, clause);
    let errors = verify(&m);
    assert!(
        errors.iter().any(|e| e.contains("clause fn3 for effect20.0 has type (i32) -> (), but should have (i32, cont() -> ()) -> ()")),
        "{errors:?}"
    );
}

#[test]
fn handle_and_perform() {
    // F: let body = closure G(); handle SUM(body); ret()
    // G: perform YIELD.0(1); ret()
    let mut m = handler_module(|_, k| resume(k, ret(vec![])));
    let mut g = Func::new(vec![], vec![]);
    g.row = Row::one(YIELD);
    g.body = let_(vec![], Op::Perform(YIELD, 0), vec![int(1)], ret(vec![]));
    m.funcs.insert(G, g);

    let mut f = Func::new(vec![], vec![]);
    let body_sig = Sig {
        row: Row::one(YIELD),
        ..unit_sig()
    };
    let body = f.new_var(Ty::Fn(Box::new(body_sig), Imm));
    f.body = let_(
        vec![body],
        Op::Closure(G, vec![], vec![]),
        vec![],
        let_(vec![], Op::Handle(SUM, vec![]), vec![v(body)], ret(vec![])),
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
    f.body = let_(vec![], Op::Call(G, vec![], vec![]), vec![], ret(vec![]));
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

/// A row can only name the effect parameters of the item it's in.
#[test]
fn effect_parameter_out_of_range() {
    let mut m = Module::default();
    let mut f = Func::new(vec![], vec![]);
    f.effect_params = 1;
    f.row = Row::param(2);
    f.body = ret(vec![]);
    m.funcs.insert(FuncId(0), f);
    let errors = verify_module(&m).unwrap_err();
    assert_eq!(
        errors[0].message,
        "E2 in row E2 isn't an effect parameter here: there is E1"
    );
}

/// An extern isn't generic, so its row can't name effect parameters. The
/// text format can't write one; a builder can.
#[test]
fn extern_row_without_parameters() {
    let mut m = Module::default();
    m.externs.insert(
        ExternId(0),
        Extern::new(
            Link::Emu { name: "f".into() },
            Sig {
                row: Row::param(1),
                ..unit_sig()
            },
        ),
    );
    let errors = verify_module(&m).unwrap_err();
    assert_eq!(
        errors[0].message,
        "E1 in row E1 isn't an effect parameter here: there are none"
    );
}

/// An export is of a function, under a name no other export of its kind
/// has: the same name is fine for two kinds.
#[test]
fn exports_name_functions_once() {
    let m = crate::text::parse(
        "#[export(wasm, name = \"x\")]
fn a() -> () {
  jump ret()
}
#[export(wasm, name = \"x\")]
fn b() -> () {
  jump ret()
}
#[export(emu, name = \"x\")]
fn c() -> () {
  jump ret()
}",
    )
    .unwrap();
    let errors: Vec<String> = verify_module(&m)
        .unwrap_err()
        .iter()
        .map(|e| e.to_string())
        .collect();
    assert_eq!(errors, ["fn1: fn1 is exported as wasm `x`, as fn0 is"]);
    let mut m = Module::default();
    m.exports.insert(
        FuncId(4),
        Export::Wasm {
            name: "gone".into(),
        },
    );
    let errors: Vec<String> = verify_module(&m)
        .unwrap_err()
        .iter()
        .map(|e| e.to_string())
        .collect();
    assert_eq!(errors, ["fn4: fn4 is exported, but isn't a function"]);
}
