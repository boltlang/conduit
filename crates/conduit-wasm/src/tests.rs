//! Each test builds IR, checks it with the verifier, lowers it, validates
//! the output, and runs it with Wasmtime.

use conduit_ir::verify::verify_module;
use conduit_ir::*;
use wasmtime::{Engine, Instance, Linker, Store, WasmParams, WasmResults};

use crate::emit_module;

use Ty::{I32, I64};

fn v(x: Var) -> Atom {
    Atom::Var(x)
}

fn int(n: i32) -> Atom {
    Atom::Lit(Lit::I32(n))
}

fn let_(binds: &[Var], op: Op, args: Vec<Atom>, body: Term) -> Term {
    Term::Let {
        binds: binds.to_vec(),
        op,
        args,
        body: Box::new(body),
        source: None,
    }
}

fn prim(bind: Var, p: Prim, args: Vec<Atom>, body: Term) -> Term {
    let_(&[bind], Op::Prim(p), args, body)
}

fn jump(target: Label, args: Vec<Atom>) -> Term {
    Term::Jump {
        target,
        args,
        source: None,
    }
}

fn ret(args: Vec<Atom>) -> Term {
    jump(Label::RET, args)
}

fn case(scrut: Atom, arms: Vec<(Tag, Term)>, default: Option<Term>) -> Term {
    Term::Case {
        scrut,
        arms: arms
            .into_iter()
            .map(|(tag, body)| Arm { tag, body })
            .collect(),
        default: default.map(Box::new),
    }
}

fn join(rec: bool, defs: Vec<(Label, Vec<Var>, Term)>, body: Term) -> Term {
    Term::Join {
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
    }
}

fn module(funcs: Vec<(FuncId, Func)>) -> Module {
    let mut m = Module::default();
    m.funcs.extend(funcs);
    m
}

/// Verifies, lowers and validates a module.
fn compile(m: &Module, exports: &[(FuncId, &str)]) -> Vec<u8> {
    if let Err(errors) = verify_module(m) {
        panic!("the test's IR doesn't verify: {errors:#?}");
    }
    let bytes = emit_module(m, exports).unwrap();
    if let Err(e) = wasmparser::Validator::new().validate_all(&bytes) {
        let wat = wasmprinter::print_bytes(&bytes).unwrap_or_default();
        panic!("invalid Wasm: {e}\n{wat}");
    }
    bytes
}

struct Wasm {
    store: Store<()>,
    instance: Instance,
}

impl Wasm {
    fn new(m: &Module, exports: &[(FuncId, &str)]) -> Wasm {
        let bytes = compile(m, exports);
        let mut config = wasmtime::Config::new();
        config
            .wasm_gc(true)
            .wasm_function_references(true)
            .wasm_tail_call(true);
        let engine = Engine::new(&config).unwrap();
        let module = wasmtime::Module::new(&engine, &bytes).unwrap();
        let mut linker = Linker::new(&engine);
        linker.func_wrap("env", "double", |x: i32| x * 2).unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &module).unwrap();
        Wasm { store, instance }
    }

    fn call<P: WasmParams, R: WasmResults>(&mut self, name: &str, args: P) -> R {
        let f = self
            .instance
            .get_typed_func::<P, R>(&mut self.store, name)
            .unwrap();
        f.call(&mut self.store, args).unwrap()
    }
}

const FAC: FuncId = FuncId(0);

/// `fac` from the design doc: a join point shared by two arms, and a call
/// that isn't a tail call.
#[test]
fn factorial() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let n = f.params[0];
    let one = f.new_label();
    let k = f.new_var(I32);
    let r = f.new_var(I32);
    let m = f.new_var(I32);
    let step = prim(
        k,
        Prim::Sub,
        vec![v(n), int(1)],
        let_(
            &[r],
            Op::Call(FAC),
            vec![v(k)],
            prim(m, Prim::Mul, vec![v(n), v(r)], ret(vec![v(m)])),
        ),
    );
    f.body = join(
        false,
        vec![(one, vec![], ret(vec![int(1)]))],
        case(
            v(n),
            vec![
                (Tag::Int(0), jump(one, vec![])),
                (Tag::Int(1), jump(one, vec![])),
            ],
            Some(step),
        ),
    );
    let mut w = Wasm::new(&module(vec![(FAC, f)]), &[(FAC, "fac")]);
    assert_eq!(w.call::<_, i32>("fac", (0,)), 1);
    assert_eq!(w.call::<_, i32>("fac", (1,)), 1);
    assert_eq!(w.call::<_, i32>("fac", (5,)), 120);
}

/// `sum` from the design doc: a loop as a recursive join point.
#[test]
fn sum_loop() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let n = f.params[0];
    let lp = f.new_label();
    let [i, acc, done, i2, acc2] = [(); 5].map(|_| f.new_var(I32));
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
        case(v(done), vec![(Tag::Int(1), ret(vec![v(acc)]))], Some(step)),
    );
    f.body = join(
        true,
        vec![(lp, vec![i, acc], body)],
        jump(lp, vec![int(0), int(0)]),
    );
    let mut w = Wasm::new(&module(vec![(FuncId(0), f)]), &[(FuncId(0), "sum")]);
    assert_eq!(w.call::<_, i32>("sum", (0,)), 0);
    assert_eq!(w.call::<_, i32>("sum", (5,)), 10);
}

/// A jump that swaps two parameters must not overwrite one before reading
/// it.
#[test]
fn jump_swaps_parameters() {
    let mut f = Func::new(vec![I32, I32, I32], vec![I32]);
    let [a, b, n] = [f.params[0], f.params[1], f.params[2]];
    let lp = f.new_label();
    let [x, y, i, d, i2] = [(); 5].map(|_| f.new_var(I32));
    let body = case(
        v(i),
        vec![(
            Tag::Int(0),
            prim(d, Prim::Sub, vec![v(x), v(y)], ret(vec![v(d)])),
        )],
        Some(prim(
            i2,
            Prim::Sub,
            vec![v(i), int(1)],
            jump(lp, vec![v(y), v(x), v(i2)]),
        )),
    );
    f.body = join(
        true,
        vec![(lp, vec![x, y, i], body)],
        jump(lp, vec![v(a), v(b), v(n)]),
    );
    let mut w = Wasm::new(&module(vec![(FuncId(0), f)]), &[(FuncId(0), "f")]);
    assert_eq!(w.call::<_, i32>("f", (10, 3, 0)), 7);
    assert_eq!(w.call::<_, i32>("f", (10, 3, 1)), -7);
    assert_eq!(w.call::<_, i32>("f", (10, 3, 2)), 7);
}

/// Two mutually recursive join points.
#[test]
fn even_odd() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let n = f.params[0];
    let even = f.new_label();
    let odd = f.new_label();
    let [k1, k1m, k2, k2m] = [(); 4].map(|_| f.new_var(I32));
    let step = |k, km, result: i32, other| {
        case(
            v(k),
            vec![(Tag::Int(0), ret(vec![int(result)]))],
            Some(prim(
                km,
                Prim::Sub,
                vec![v(k), int(1)],
                jump(other, vec![v(km)]),
            )),
        )
    };
    f.body = join(
        true,
        vec![
            (even, vec![k1], step(k1, k1m, 1, odd)),
            (odd, vec![k2], step(k2, k2m, 0, even)),
        ],
        jump(even, vec![v(n)]),
    );
    let mut w = Wasm::new(&module(vec![(FuncId(0), f)]), &[(FuncId(0), "is_even")]);
    assert_eq!(w.call::<_, i32>("is_even", (0,)), 1);
    assert_eq!(w.call::<_, i32>("is_even", (7,)), 0);
    assert_eq!(w.call::<_, i32>("is_even", (10,)), 1);
}

/// `let r = call f(..); jump ret(r)` is a `return_call`, so deep recursion
/// doesn't grow the stack.
#[test]
fn tail_calls_dont_grow_the_stack() {
    let id = FuncId(0);
    let mut f = Func::new(vec![I32, I32], vec![I32]);
    let [n, acc] = [f.params[0], f.params[1]];
    let [n2, acc2, r] = [(); 3].map(|_| f.new_var(I32));
    f.body = case(
        v(n),
        vec![(Tag::Int(0), ret(vec![v(acc)]))],
        Some(prim(
            n2,
            Prim::Sub,
            vec![v(n), int(1)],
            prim(
                acc2,
                Prim::Add,
                vec![v(acc), int(2)],
                let_(&[r], Op::Call(id), vec![v(n2), v(acc2)], ret(vec![v(r)])),
            ),
        )),
    );
    let m = module(vec![(id, f)]);
    let wat = wasmprinter::print_bytes(compile(&m, &[])).unwrap();
    assert!(wat.contains("return_call"), "{wat}");
    let mut w = Wasm::new(&m, &[(id, "count")]);
    assert_eq!(w.call::<_, i32>("count", (10_000_000, 0)), 20_000_000);
}

/// A dense case uses `br_table`, a sparse one compares each tag.
#[test]
fn integer_cases() {
    let mut dense = Func::new(vec![I32], vec![I32]);
    let n = dense.params[0];
    let arms = [0, 1, 2, 3, 5]
        .into_iter()
        .map(|t| (Tag::Int(t), ret(vec![int(10 + t as i32)])))
        .collect();
    dense.body = case(v(n), arms, Some(ret(vec![int(99)])));

    let mut sparse = Func::new(vec![I32], vec![I32]);
    let n = sparse.params[0];
    let arms = [-1000, 7, 1 << 20]
        .into_iter()
        .zip(1..)
        .map(|(t, r)| (Tag::Int(t), ret(vec![int(r)])))
        .collect();
    sparse.body = case(v(n), arms, Some(ret(vec![int(0)])));

    let mut wide = Func::new(vec![I64], vec![I64]);
    let n = wide.params[0];
    wide.body = case(
        v(n),
        vec![(Tag::Int(1 << 40), ret(vec![Atom::Lit(Lit::I64(1))]))],
        Some(ret(vec![Atom::Lit(Lit::I64(2))])),
    );

    let m = module(vec![
        (FuncId(0), dense),
        (FuncId(1), sparse),
        (FuncId(2), wide),
    ]);
    let exports = [
        (FuncId(0), "dense"),
        (FuncId(1), "sparse"),
        (FuncId(2), "wide"),
    ];
    let wat = wasmprinter::print_bytes(compile(&m, &exports)).unwrap();
    assert!(wat.contains("br_table"), "{wat}");
    let mut w = Wasm::new(&m, &exports);
    for (x, expected) in [
        (-1, 99),
        (0, 10),
        (3, 13),
        (4, 99),
        (5, 15),
        (6, 99),
        (i32::MIN, 99),
    ] {
        assert_eq!(w.call::<_, i32>("dense", (x,)), expected, "dense({x})");
    }
    for (x, expected) in [(-1000, 1), (7, 2), (1 << 20, 3), (8, 0)] {
        assert_eq!(w.call::<_, i32>("sparse", (x,)), expected, "sparse({x})");
    }
    assert_eq!(w.call::<_, i64>("wide", (1i64 << 40,)), 1);
    assert_eq!(w.call::<_, i64>("wide", (1i64,)), 2);
}

#[test]
fn prims() {
    // -(i32(f64(x) * 2.5))
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let d = f.new_var(Ty::F64);
    let e = f.new_var(Ty::F64);
    let r = f.new_var(I32);
    let neg = f.new_var(I32);
    f.body = prim(
        d,
        Prim::Convert(Ty::F64, Sign::Signed),
        vec![v(x)],
        prim(
            e,
            Prim::Mul,
            vec![v(d), Atom::Lit(Lit::f64(2.5))],
            prim(
                r,
                Prim::Convert(I32, Sign::Signed),
                vec![v(e)],
                prim(neg, Prim::Neg, vec![v(r)], ret(vec![v(neg)])),
            ),
        ),
    );

    let mut lt = Func::new(vec![I32], vec![I32]);
    let x = lt.params[0];
    let c = lt.new_var(I32);
    lt.body = prim(
        c,
        Prim::Lt(Sign::Unsigned),
        vec![v(x), int(1)],
        ret(vec![v(c)]),
    );

    let m = module(vec![(FuncId(0), f), (FuncId(1), lt)]);
    let mut w = Wasm::new(&m, &[(FuncId(0), "f"), (FuncId(1), "lt_u")]);
    assert_eq!(w.call::<_, i32>("f", (4,)), -10);
    assert_eq!(w.call::<_, i32>("lt_u", (0,)), 1);
    assert_eq!(w.call::<_, i32>("lt_u", (-1,)), 0);
}

const LIST: DataId = DataId(0);
const NIL: CtorId = CtorId {
    data: LIST,
    index: 0,
};
const CONS: CtorId = CtorId {
    data: LIST,
    index: 1,
};
const PAIR: DataId = DataId(1);
const PAIR_C: CtorId = CtorId {
    data: PAIR,
    index: 0,
};

fn with_data(mut m: Module) -> Module {
    m.data.insert(
        LIST,
        DataDecl {
            ctors: vec![
                CtorDecl { fields: vec![] },
                CtorDecl {
                    fields: vec![I32, Ty::Data(LIST, Mutability::Inherit)],
                },
            ],
        },
    );
    m.data.insert(
        PAIR,
        DataDecl {
            ctors: vec![CtorDecl {
                fields: vec![I32, I32],
            }],
        },
    );
    m
}

/// Builds a list, matches on it, and reads its fields.
#[test]
fn lists() {
    let list = Ty::Data(LIST, Mutability::Imm);
    let (build_id, sum_id, main_id, is_nil_id) = (FuncId(0), FuncId(1), FuncId(2), FuncId(3));

    // build(n) = if n == 0 { Nil } else { Cons(n, build(n - 1)) }
    let mut build = Func::new(vec![I32], vec![list.clone()]);
    let n = build.params[0];
    let nil = build.new_var(list.clone());
    let k = build.new_var(I32);
    let tail = build.new_var(list.clone());
    let cons = build.new_var(list.clone());
    build.body = case(
        v(n),
        vec![(
            Tag::Int(0),
            let_(&[nil], Op::New(NIL), vec![], ret(vec![v(nil)])),
        )],
        Some(prim(
            k,
            Prim::Sub,
            vec![v(n), int(1)],
            let_(
                &[tail],
                Op::Call(build_id),
                vec![v(k)],
                let_(
                    &[cons],
                    Op::New(CONS),
                    vec![v(n), v(tail)],
                    ret(vec![v(cons)]),
                ),
            ),
        )),
    );

    // sum(l) = case l { Nil => 0, Cons => l.0 + sum(l.1) }
    let mut sum = Func::new(vec![list.clone()], vec![I32]);
    let l = sum.params[0];
    let x = sum.new_var(I32);
    let t = sum.new_var(list.clone());
    let s = sum.new_var(I32);
    let r = sum.new_var(I32);
    sum.body = case(
        v(l),
        vec![
            (Tag::Ctor(NIL), ret(vec![int(0)])),
            (
                Tag::Ctor(CONS),
                let_(
                    &[x],
                    Op::Get(CONS, 0),
                    vec![v(l)],
                    let_(
                        &[t],
                        Op::Get(CONS, 1),
                        vec![v(l)],
                        let_(
                            &[s],
                            Op::Call(sum_id),
                            vec![v(t)],
                            prim(r, Prim::Add, vec![v(x), v(s)], ret(vec![v(r)])),
                        ),
                    ),
                ),
            ),
        ],
        None,
    );

    // main(n) = sum(build(n))
    let mut main = Func::new(vec![I32], vec![I32]);
    let n = main.params[0];
    let l = main.new_var(list.clone());
    let r = main.new_var(I32);
    main.body = let_(
        &[l],
        Op::Call(build_id),
        vec![v(n)],
        let_(&[r], Op::Call(sum_id), vec![v(l)], ret(vec![v(r)])),
    );

    // is_nil(n) = case build(n) { Cons => 0, _ => 1 }
    let mut is_nil = Func::new(vec![I32], vec![I32]);
    let n = is_nil.params[0];
    let l = is_nil.new_var(list);
    is_nil.body = let_(
        &[l],
        Op::Call(build_id),
        vec![v(n)],
        case(
            v(l),
            vec![(Tag::Ctor(CONS), ret(vec![int(0)]))],
            Some(ret(vec![int(1)])),
        ),
    );

    let m = with_data(module(vec![
        (build_id, build),
        (sum_id, sum),
        (main_id, main),
        (is_nil_id, is_nil),
    ]));
    let mut w = Wasm::new(&m, &[(main_id, "main"), (is_nil_id, "is_nil")]);
    assert_eq!(w.call::<_, i32>("main", (0,)), 0);
    assert_eq!(w.call::<_, i32>("main", (10,)), 55);
    assert_eq!(w.call::<_, i32>("is_nil", (0,)), 1);
    assert_eq!(w.call::<_, i32>("is_nil", (3,)), 0);
}

/// A struct: one constructor, so no tag, and a field written in place.
#[test]
fn pairs() {
    let pair = Ty::Data(PAIR, Mutability::Mut);
    let mut f = Func::new(vec![I32, I32], vec![I32]);
    let [a, b] = [f.params[0], f.params[1]];
    let p = f.new_var(pair);
    let [x, y, d] = [(); 3].map(|_| f.new_var(I32));
    f.body = let_(
        &[p],
        Op::New(PAIR_C),
        vec![v(a), v(b)],
        let_(
            &[],
            Op::Set(PAIR_C, 0),
            vec![v(p), int(10)],
            let_(
                &[x],
                Op::Get(PAIR_C, 0),
                vec![v(p)],
                let_(
                    &[y],
                    Op::Get(PAIR_C, 1),
                    vec![v(p)],
                    prim(d, Prim::Sub, vec![v(x), v(y)], ret(vec![v(d)])),
                ),
            ),
        ),
    );
    let mut w = Wasm::new(
        &with_data(module(vec![(FuncId(0), f)])),
        &[(FuncId(0), "f")],
    );
    assert_eq!(w.call::<_, i32>("f", (1, 3)), 7);
}

#[test]
fn arrays() {
    // a = [7; n]; a[1] = 5; b = [1, 2, 3]; a[1] + len(a) + b[2]
    let mut f = Func::new(vec![I32], vec![I32]);
    let n = f.params[0];
    let a = f.new_var(Ty::Array(Box::new(I32), Mutability::Mut));
    let b = f.new_var(Ty::Array(Box::new(I32), Mutability::Imm));
    let [x, len, y, s1, s] = [(); 5].map(|_| f.new_var(I32));
    f.body = let_(
        &[a],
        Op::ArrayFill,
        vec![v(n), int(7)],
        let_(
            &[],
            Op::ArraySet,
            vec![v(a), int(1), int(5)],
            let_(
                &[x],
                Op::ArrayGet,
                vec![v(a), int(1)],
                let_(
                    &[len],
                    Op::ArrayLen,
                    vec![v(a)],
                    let_(
                        &[b],
                        Op::ArrayNew,
                        vec![int(1), int(2), int(3)],
                        let_(
                            &[y],
                            Op::ArrayGet,
                            vec![v(b), int(2)],
                            prim(
                                s1,
                                Prim::Add,
                                vec![v(x), v(len)],
                                prim(s, Prim::Add, vec![v(s1), v(y)], ret(vec![v(s)])),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    );
    let mut w = Wasm::new(&module(vec![(FuncId(0), f)]), &[(FuncId(0), "f")]);
    assert_eq!(w.call::<_, i32>("f", (10,)), 18);
}

/// The `var i` loop from rule 6 of the design doc, with its ref not
/// promoted.
#[test]
fn refs() {
    let mut f = Func::new(vec![I32], vec![I32]);
    let n = f.params[0];
    let i = f.new_var(Ty::Ref(Box::new(I32)));
    let lp = f.new_label();
    let [x, c, x2, r] = [(); 4].map(|_| f.new_var(I32));
    let body = let_(
        &[x],
        Op::RefGet,
        vec![v(i)],
        prim(
            c,
            Prim::Lt(Sign::Signed),
            vec![v(x), v(n)],
            case(
                v(c),
                vec![(
                    Tag::Int(1),
                    prim(
                        x2,
                        Prim::Add,
                        vec![v(x), int(1)],
                        let_(&[], Op::RefSet, vec![v(i), v(x2)], jump(lp, vec![])),
                    ),
                )],
                Some(let_(&[r], Op::RefGet, vec![v(i)], ret(vec![v(r)]))),
            ),
        ),
    );
    f.body = let_(
        &[i],
        Op::RefNew,
        vec![int(0)],
        join(true, vec![(lp, vec![], body)], jump(lp, vec![])),
    );
    let mut w = Wasm::new(&module(vec![(FuncId(0), f)]), &[(FuncId(0), "f")]);
    assert_eq!(w.call::<_, i32>("f", (7,)), 7);
}

/// Two closures of the same signature with different captures, applied
/// through the same join point.
#[test]
fn closures() {
    let fn_ty = Ty::Fn(
        Box::new(Sig {
            params: vec![I32],
            results: vec![I32],
        }),
        Mutability::Imm,
    );
    let (adder_id, mul_id, pick_id) = (FuncId(0), FuncId(1), FuncId(2));
    let lifted = |p: Prim| {
        let mut f = Func::new(vec![I32, I32], vec![I32]);
        let [n, x] = [f.params[0], f.params[1]];
        let r = f.new_var(I32);
        f.body = prim(r, p, vec![v(x), v(n)], ret(vec![v(r)]));
        f
    };

    // pick(b, n, x) = (if b { |x| x + n } else { |x| x * n })(x) + 1
    let mut pick = Func::new(vec![I32, I32, I32], vec![I32]);
    let [b, n, x] = [pick.params[0], pick.params[1], pick.params[2]];
    let k = pick.new_label();
    let c = pick.new_var(fn_ty.clone());
    let c1 = pick.new_var(fn_ty.clone());
    let c2 = pick.new_var(fn_ty);
    let r = pick.new_var(I32);
    let s = pick.new_var(I32);
    pick.body = join(
        false,
        vec![(
            k,
            vec![c],
            let_(
                &[r],
                Op::Apply,
                vec![v(c), v(x)],
                prim(s, Prim::Add, vec![v(r), int(1)], ret(vec![v(s)])),
            ),
        )],
        case(
            v(b),
            vec![(
                Tag::Int(1),
                let_(
                    &[c1],
                    Op::Closure(adder_id),
                    vec![v(n)],
                    jump(k, vec![v(c1)]),
                ),
            )],
            Some(let_(
                &[c2],
                Op::Closure(mul_id),
                vec![v(n)],
                jump(k, vec![v(c2)]),
            )),
        ),
    );

    let m = module(vec![
        (adder_id, lifted(Prim::Add)),
        (mul_id, lifted(Prim::Mul)),
        (pick_id, pick),
    ]);
    let mut w = Wasm::new(&m, &[(pick_id, "pick")]);
    assert_eq!(w.call::<_, i32>("pick", (1, 3, 4)), 8);
    assert_eq!(w.call::<_, i32>("pick", (0, 3, 4)), 13);
}

/// A closure applied in tail position is a `return_call_ref`.
#[test]
fn closure_tail_call() {
    let sig = Sig {
        params: vec![I32],
        results: vec![I32],
    };
    let fn_ty = Ty::Fn(Box::new(sig), Mutability::Imm);
    let (adder_id, main_id) = (FuncId(0), FuncId(1));
    let mut adder = Func::new(vec![I32, I32], vec![I32]);
    let [n, x] = [adder.params[0], adder.params[1]];
    let r = adder.new_var(I32);
    adder.body = prim(r, Prim::Add, vec![v(x), v(n)], ret(vec![v(r)]));

    let mut main = Func::new(vec![I32, I32], vec![I32]);
    let [n, x] = [main.params[0], main.params[1]];
    let c = main.new_var(fn_ty);
    let r = main.new_var(I32);
    main.body = let_(
        &[c],
        Op::Closure(adder_id),
        vec![v(n)],
        let_(&[r], Op::Apply, vec![v(c), v(x)], ret(vec![v(r)])),
    );
    let m = module(vec![(adder_id, adder), (main_id, main)]);
    let wat = wasmprinter::print_bytes(compile(&m, &[])).unwrap();
    assert!(wat.contains("return_call_ref"), "{wat}");
    let mut w = Wasm::new(&m, &[(main_id, "main")]);
    assert_eq!(w.call::<_, i32>("main", (3, 4)), 7);
}

/// Boxing a scalar allocates; boxing an object is a no-op.
#[test]
fn boxes() {
    let pair = Ty::Data(PAIR, Mutability::Imm);
    let any = Ty::Any(Mutability::Imm);
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let b = f.new_var(any.clone());
    let y = f.new_var(I32);
    let p = f.new_var(pair.clone());
    let bp = f.new_var(any);
    let q = f.new_var(pair.clone());
    let z = f.new_var(I32);
    f.body = let_(
        &[b],
        Op::Box(I32),
        vec![v(x)],
        let_(
            &[y],
            Op::Unbox(I32),
            vec![v(b)],
            let_(
                &[p],
                Op::New(PAIR_C),
                vec![v(y), int(1)],
                let_(
                    &[bp],
                    Op::Box(pair.clone()),
                    vec![v(p)],
                    let_(
                        &[q],
                        Op::Unbox(pair),
                        vec![v(bp)],
                        let_(&[z], Op::Get(PAIR_C, 0), vec![v(q)], ret(vec![v(z)])),
                    ),
                ),
            ),
        ),
    );
    let mut w = Wasm::new(
        &with_data(module(vec![(FuncId(0), f)])),
        &[(FuncId(0), "f")],
    );
    assert_eq!(w.call::<_, i32>("f", (42,)), 42);
}

#[test]
fn externs() {
    let ext = ExternId(0);
    let mut f = Func::new(vec![I32], vec![I32]);
    let x = f.params[0];
    let d = f.new_var(I32);
    let r = f.new_var(I32);
    f.body = let_(
        &[d],
        Op::CallExtern(ext),
        vec![v(x)],
        prim(r, Prim::Add, vec![v(d), int(1)], ret(vec![v(r)])),
    );
    let mut m = module(vec![(FuncId(0), f)]);
    m.externs.insert(
        ext,
        Extern {
            module: "env".into(),
            name: "double".into(),
            sig: Sig {
                params: vec![I32],
                results: vec![I32],
            },
        },
    );
    let mut w = Wasm::new(&m, &[(FuncId(0), "f")]);
    assert_eq!(w.call::<_, i32>("f", (20,)), 41);
}

#[test]
fn copy_is_not_supported_yet() {
    let mut f = Func::new(
        vec![Ty::Data(PAIR, Mutability::Mut)],
        vec![Ty::Data(PAIR, Mutability::Imm)],
    );
    let p = f.params[0];
    let q = f.new_var(Ty::Data(PAIR, Mutability::Imm));
    f.body = let_(&[q], Op::Copy(Mutability::Imm), vec![v(p)], ret(vec![v(q)]));
    let m = with_data(module(vec![(FuncId(0), f)]));
    verify_module(&m).unwrap();
    let err = emit_module(&m, &[]).unwrap_err();
    assert!(err.message.contains("copy"), "{err}");
}

#[test]
fn output_is_deterministic() {
    let mut f = Func::new(vec![I32], vec![Ty::Data(LIST, Mutability::Imm)]);
    let n = f.params[0];
    let nil = f.new_var(Ty::Data(LIST, Mutability::Imm));
    let cons = f.new_var(Ty::Data(LIST, Mutability::Imm));
    f.body = let_(
        &[nil],
        Op::New(NIL),
        vec![],
        let_(
            &[cons],
            Op::New(CONS),
            vec![v(n), v(nil)],
            ret(vec![v(cons)]),
        ),
    );
    let m = with_data(module(vec![(FuncId(0), f)]));
    assert_eq!(compile(&m, &[]), compile(&m, &[]));
}
