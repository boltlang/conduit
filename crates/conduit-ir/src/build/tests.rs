//! Each test builds a function and compares its text with what's
//! expected. Functions that should be correct also pass the verifier.

use super::*;
use crate::text::{parse, print_func};

use Ty::I32;

const F: FuncId = FuncId(0);

/// A module with these declarations and a placeholder for `F`, so that
/// `F` can call itself.
fn module(decls: &str, params: Vec<Ty>, results: Vec<Ty>) -> Module {
    let mut m = parse(decls).unwrap_or_else(|e| panic!("{e}"));
    m.funcs.insert(F, Func::new(params, results));
    m
}

/// Finishes `f`, verifies it, and gives its text.
fn text(f: FuncBuilder<'_, Module>) -> String {
    let func = f.finish_verified(F).unwrap_or_else(|e| panic!("{e:#?}"));
    print_func(F, &func)
}

fn messages(f: FuncBuilder<'_, Module>) -> Vec<String> {
    match f.finish() {
        Ok(func) => panic!("expected errors, but built:\n{}", print_func(F, &func)),
        Err(errors) => errors.into_iter().map(|e| e.to_string()).collect(),
    }
}

const LIST: &str = "
data data0 {
  ()
  (i32, inherit data0)
}
";
const NIL: CtorId = CtorId {
    data: DataId(0),
    index: 0,
};
const CONS: CtorId = CtorId {
    data: DataId(0),
    index: 1,
};

/// `let x = if c { a } else { b }; x + 1`, emitted in the order a
/// frontend walks it: the join point first, then the branches, then the
/// code after.
#[test]
fn if_else() {
    let m = module("", vec![I32; 3], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32; 3], vec![I32]);
    let [c, a, b] = f.params();
    let k = f.join(&[I32]);
    let case = f.case(c);
    f.arm(case, 1);
    f.jump(&k, [a]);
    f.default(case);
    f.jump(&k, [b]);
    f.switch_to(k.block());
    let y = f.add(k.param(0), 1i32);
    f.ret([y]);
    assert_eq!(
        text(f),
        "fn fn0(%0: i32, %1: i32, %2: i32) -> (i32) {
  join j1(%3: i32) =
    let %4: i32 = add(%3, 1i32)
    jump ret(%4)
  in
  case %0 {
    1 => jump j1(%1)
    _ => jump j1(%2)
  }
}"
    );
}

/// `fac` from docs/ir.md: one arm for the tags 0 and 1, which the builder
/// turns into a join point, and a recursive call.
#[test]
fn factorial() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [n] = f.params();
    let case = f.case(n);
    f.arms(case, [0, 1]);
    f.ret([1i32]);
    f.default(case);
    let k = f.sub(n, 1i32);
    let r = f.call1(F, [k]);
    let p = f.mul(n, r);
    f.ret([p]);
    assert_eq!(
        text(f),
        "fn fn0(%0: i32) -> (i32) {
  join j1() = jump ret(1i32)
  in
  case %0 {
    0 => jump j1()
    1 => jump j1()
    _ =>
      let %1: i32 = sub(%0, 1i32)
      let %2: i32 = call fn0(%1)
      let %3: i32 = mul(%0, %2)
      jump ret(%3)
  }
}"
    );
}

/// A loop: a recursive join point that jumps to itself.
#[test]
fn sum_loop() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [n] = f.params();
    let lp = f.join_rec(&[I32, I32]);
    f.jump(&lp, [Atom::from(0i32), 0i32.into()]);
    f.switch_to(lp.block());
    let (i, acc) = (lp.param(0), lp.param(1));
    let done = f.ge_s(i, n);
    let case = f.case(done);
    f.arm(case, 1);
    f.ret([acc]);
    f.default(case);
    let i2 = f.add(i, 1i32);
    let acc2 = f.add(acc, i);
    f.jump(&lp, [i2, acc2]);
    assert_eq!(
        text(f),
        "fn fn0(%0: i32) -> (i32) {
  join rec j1(%1: i32, %2: i32) =
    let %3: i32 = ge.s(%1, %0)
    case %3 {
      1 => jump ret(%2)
      _ =>
        let %4: i32 = add(%1, 1i32)
        let %5: i32 = add(%2, %1)
        jump j1(%4, %5)
    }
  in jump j1(0i32, 0i32)
}"
    );
}

/// Matching on a list: arms per constructor, and fields read in them, with
/// their types inferred from the data type.
#[test]
fn match_on_a_list() {
    let list = Ty::data(DataId(0), Mutability::Imm);
    let m = module(LIST, vec![list.clone()], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![list.clone()], vec![I32]);
    let [l] = f.params();
    let case = f.case(l);
    f.arm(case, NIL);
    f.ret([0i32]);
    f.arm(case, CONS);
    let [x, rest] = f.fields(CONS, l)[..] else {
        panic!("Cons has two fields")
    };
    assert_eq!(f.var_ty(rest), Some(&list));
    let s = f.call1(F, [rest]);
    let r = f.add(x, s);
    f.ret([r]);
    assert_eq!(
        text(f),
        "fn fn0(%0: imm data0) -> (i32) {
  case %0 {
    data0.0 => jump ret(0i32)
    data0.1 =>
      let %1: i32 = get data0.1.0(%0)
      let %2: imm data0 = get data0.1.1(%0)
      let %3: i32 = call fn0(%2)
      let %4: i32 = add(%1, %3)
      jump ret(%4)
  }
}"
    );
}

/// `Some(x) if x > 0 => x, _ => 0`: a guard falls through to a join point.
#[test]
fn guard_falls_through() {
    let option = "
data data0 {
  ()
  (i32)
}
";
    let some = CtorId {
        data: DataId(0),
        index: 1,
    };
    let opt = Ty::data(DataId(0), Mutability::Imm);
    let m = module(option, vec![opt.clone()], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![opt], vec![I32]);
    let [o] = f.params();
    let fail = f.join(&[]);
    let case = f.case(o);
    f.arm(case, some);
    let x = f.get(some, 0, o);
    let positive = f.gt_s(x, 0i32);
    let guard = f.case(positive);
    f.arm(guard, 1);
    f.ret([x]);
    f.default(guard);
    f.jump(&fail, ());
    f.default(case);
    f.jump(&fail, ());
    f.switch_to(fail.block());
    f.ret([0i32]);
    assert_eq!(
        text(f),
        "fn fn0(%0: imm data0) -> (i32) {
  join j1() = jump ret(0i32)
  in
  case %0 {
    data0.1 =>
      let %1: i32 = get data0.1.0(%0)
      let %2: i32 = gt.s(%1, 0i32)
      case %2 {
        1 => jump ret(%1)
        _ => jump j1()
      }
    _ => jump j1()
  }
}"
    );
}

/// Objects, arrays, refs, closures and boxes, with their result types.
#[test]
fn objects() {
    let pair = "
data data0 {
  (i32, i32)
}

fn fn1(%0: i32, %1: i32) -> (i32) {
  jump ret(%0)
}
";
    let ctor = CtorId {
        data: DataId(0),
        index: 0,
    };
    let m = module(pair, vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [x] = f.params();
    let p = f.new_object(ctor, Mutability::Mut, [x, x]);
    f.set(ctor, 1, p, 2i32);
    let a = f.array_fill(I32, Mutability::Mut, 3i32, x);
    f.array_set(a, 0i32, 5i32);
    let len = f.array_len(a);
    let r = f.ref_new(len);
    let v = f.ref_get(r);
    let c = f.closure(FuncId(1), Mutability::Imm, [v]);
    let y = f.apply1(c, [x]);
    let b = f.boxed(I32, y);
    let z = f.unbox(I32, b);
    f.ret([z]);
    assert_eq!(
        text(f),
        "fn fn0(%0: i32) -> (i32) {
  let %1: mut data0 = new data0.0(%0, %0)
  let = set data0.0.1(%1, 2i32)
  let %2: mut [i32] = array_fill(3i32, %0)
  let = array_set(%2, 0i32, 5i32)
  let %3: i32 = array_len(%2)
  let %4: ref i32 = ref_new(%3)
  let %5: i32 = ref_get(%4)
  let %6: imm fn(i32) -> (i32) = closure fn1(%5)
  let %7: i32 = apply(%6, %0)
  let %8: imm any = box[i32](%7)
  let %9: i32 = unbox[i32](%8)
  jump ret(%9)
}"
    );
}

#[test]
fn sources() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [x] = f.params();
    f.set_source(Some(SourceId(7)));
    let y = f.add(x, 1i32);
    f.set_source(Some(SourceId(8)));
    f.ret([y]);
    assert_eq!(
        text(f),
        "fn fn0(%0: i32) -> (i32) {
  let %1: i32 = add(%0, 1i32) @src7
  jump ret(%1) @src8
}"
    );
}

/// A function as long as this doesn't need a deep stack to build.
#[test]
fn long_functions() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [mut x] = f.params();
    for _ in 0..100_000 {
        x = f.add(x, 1i32);
    }
    f.ret([x]);
    let func = f.finish().unwrap();
    assert_eq!(func.body.lets.len(), 100_000);
}

#[test]
fn reports_type_errors() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [x] = f.params();
    f.set_source(Some(SourceId(3)));
    let y = f.add(x, 1i64);
    f.ret([y]);
    assert_eq!(
        messages(f),
        ["src3: Add needs two numbers of the same type, but got (i32, i64)"]
    );
}

#[test]
fn reports_jumps_with_the_wrong_arguments() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let k = f.join(&[I32, I32]);
    f.jump(&k, [1i32]);
    f.switch_to(k.block());
    f.ret([2i64]);
    assert_eq!(
        messages(f),
        [
            "jump to j1: expected 2 values, but got 1",
            "jump to ret: value 0 has type i64, but should have i32",
        ]
    );
}

#[test]
fn reports_fields_of_unknown_constructors() {
    let list = Ty::data(DataId(0), Mutability::Imm);
    let m = module(LIST, vec![list.clone()], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![list], vec![I32]);
    let [l] = f.params();
    let x = f.get(CONS, 0, l);
    f.ret([x]);
    assert_eq!(
        messages(f),
        ["%0 isn't known to be built by data0.1 here, so its fields can't be accessed"]
    );
}

#[test]
fn reports_bad_arms() {
    let list = Ty::data(DataId(0), Mutability::Imm);
    let m = module(LIST, vec![list.clone(), I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![list, I32], vec![I32]);
    let [l, n] = f.params();
    let on_list = f.case(l);
    f.arm(on_list, CONS);
    f.ret([1i32]);
    f.arm(on_list, CONS);
    f.ret([2i32]);
    f.arm(on_list, 3);
    f.ret([3i32]);
    f.default(on_list);
    let on_int = f.case(n);
    f.arm(on_int, 1i64 << 40);
    f.ret([4i32]);
    assert_eq!(
        messages(f),
        [
            "the case on %0 already has an arm data0.1",
            "the case on %0 is on a imm data0, so it can't have a tag 3",
            "the case on %1 is on an i32, so it can't have a tag 1099511627776",
            "the case on %1 has no default arm and doesn't cover every value",
        ]
    );
}

#[test]
fn reports_blocks_that_never_end() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [x] = f.params();
    let k = f.join(&[]);
    let case = f.case(x);
    f.arm(case, 0);
    f.default(case);
    f.jump(&k, ());
    f.add(x, 1i32);
    assert_eq!(
        messages(f),
        [
            "a let added to the default arm of the case on %0, which has already ended: \
             switch to another block first",
            "the body of j1 never ends: it needs a jump",
            "the arm 0 of the case on %0 never ends: it needs a jump",
        ]
    );
}

#[test]
fn reports_ops_whose_result_type_cant_be_inferred() {
    let m = module(LIST, vec![], vec![]);
    let mut f = FuncBuilder::new(&m, vec![], vec![]);
    f.op(Op::New(NIL), ());
    let list = Ty::data(DataId(0), Mutability::Imm);
    let built = f.op_as(Op::New(NIL), (), vec![list.clone()]);
    assert_eq!(f.var_ty(built[0]), Some(&list));
    f.ret(());
    assert_eq!(
        messages(f),
        ["new data0.0 must bind one value of its data type: this op needs its result type given"]
    );
}

/// The builder doesn't check that a jump's label is in scope, but
/// `finish_verified` does.
#[test]
fn finish_verified_runs_the_verifier() {
    let m = module("", vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [x] = f.params();
    let case = f.case(x);
    f.arm(case, 0);
    let k = f.join(&[]);
    f.ret([1i32]);
    f.switch_to(k.block());
    f.ret([2i32]);
    f.default(case);
    f.jump(&k, ());
    let errors = f.finish_verified(F).unwrap_err();
    assert_eq!(errors[0].message, "jump to j1, which isn't in scope");
}

const DECLS: &str = "
data data0 {
  (i32)
}

extern extern1 \"env\" \"two\" (i32) -> (i32, i32)
extern extern2 \"env\" \"one\" (i32) -> (i32)

effect effect20 {
  (i32) -> (i64)
}

fn fn1(%0: i32) -> (i32, i32) {
  jump ret(%0, %0)
}
";

/// The ops `objects` doesn't build, and each kind of atom.
#[test]
fn other_ops() {
    let cell = CtorId {
        data: DataId(0),
        index: 0,
    };
    let k_ty = Ty::Cont(Box::new(Sig {
        params: vec![Ty::I64],
        results: vec![I32],
    }));
    let params = vec![I32, k_ty.clone()];
    let m = module(DECLS, params.clone(), vec![I32]);
    let mut f = FuncBuilder::new(&m, params, vec![I32]);
    let (x, k) = (f.param(0), f.param(1));
    assert_eq!(f.var_ty(k), Some(&k_ty));
    let wide = f.convert(Ty::I64, Sign::Unsigned, x);
    let bits = f.reinterpret(Ty::F64, wide);
    let half = f.mul(bits, 0.5f64);
    let small = f.convert(Ty::F32, Sign::Signed, half);
    let _ = f.add(small, 1.5f32);
    let two = f.call(FuncId(1), vec![x]);
    let from_host = f.call_extern(ExternId(1), &[two[0]][..]);
    let one = f.call_extern1(ExternId(2), [from_host[1]]);
    let lits = f.array_new(I32, Mutability::Imm, [Lit::I32(1), Lit::I32(2)]);
    let first = f.array_get(lits, 0i32);
    let r = f.ref_new(first);
    f.ref_set(r, one);
    let obj = f.new_object(cell, Mutability::Mut, [x]);
    let _ = f.get(cell, 0, obj);
    let buffer = f.array_fill(I32, Mutability::Mut, 2i32, x);
    let cloned = f.array_clone(Mutability::Imm, buffer);
    let frozen = f.freeze(buffer);
    let _ = f.array_len(cloned);
    let _ = f.array_len(frozen);
    let got = f.perform(EffectId(20), 0, [x]);
    let resumed = f.resume(k, [got[0]]);
    let start = f.current();
    let j = f.join(&[I32]);
    assert_eq!(j.params(), [j.param(0)]);
    assert_eq!(j.label(), Label(1));
    assert!(f.is_ended(start));
    let case = f.case(resumed[0]);
    f.arms(case, [7]);
    f.jump(&j, [resumed[0]]);
    f.default(case);
    f.unreachable();
    f.switch_to(j.block());
    f.ret([j.param(0)]);
    assert_eq!(
        text(f),
        "fn fn0(%0: i32, %1: cont(i64) -> (i32)) -> (i32) {
  let %2: i64 = convert.u[i64](%0)
  let %3: f64 = reinterpret[f64](%2)
  let %4: f64 = mul(%3, 0.5f64)
  let %5: f32 = convert.s[f32](%4)
  let %6: f32 = add(%5, 1.5f32)
  let %7: i32, %8: i32 = call fn1(%0)
  let %9: i32, %10: i32 = call_extern extern1(%7)
  let %11: i32 = call_extern extern2(%10)
  let %12: imm [i32] = array_new(1i32, 2i32)
  let %13: i32 = array_get(%12, 0i32)
  let %14: ref i32 = ref_new(%13)
  let = ref_set(%14, %11)
  let %15: mut data0 = new data0.0(%0)
  let %16: i32 = get data0.0.0(%15)
  let %17: mut [i32] = array_fill(2i32, %0)
  let %18: imm [i32] = array_clone(%17)
  let %19: imm [i32] = freeze(%17)
  let %20: i32 = array_len(%18)
  let %21: i32 = array_len(%19)
  let %22: i64 = perform effect20.0(%0)
  let %23: i32 = resume(%1, %22)
  join j1(%24: i32) = jump ret(%24)
  in
  case %23 {
    7 => jump j1(%23)
    _ => unreachable
  }
}"
    );
}

#[test]
fn reports_misuse() {
    let m = module(DECLS, vec![I32], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![I32], vec![I32]);
    let [x] = f.params();
    let nowhere = Var(99);
    f.add(nowhere, 1i32);
    f.op_as(Op::Prim(Prim::Add), [x, x], vec![Ty::I64]);
    f.call1(FuncId(1), [x]);
    f.closure(FuncId(99), Mutability::Imm, ());
    let on_float = f.case(1.5f64);
    f.default(on_float);
    let on_nothing = f.case(nowhere);
    f.default(on_nothing);
    f.ret([x]);
    f.default(on_nothing);
    f.ret([x]);
    let cell = Ty::data(DataId(0), Mutability::Imm);
    let mut g = FuncBuilder::new(&m, vec![cell], vec![I32]);
    let [c] = g.params();
    let on_cell = g.case(c);
    g.arm(on_cell, CONS);
    g.ret([1i32]);
    assert_eq!(
        messages(f),
        [
            "an argument is a variable this builder didn't make",
            "the op gives (i32), which doesn't fit (i64)",
            "the op gives 2 values, not one",
            "unknown function fn99",
            "can't branch on a value of type f64",
            "the case is on a variable this builder didn't make",
            "the case on %99 already has a default arm",
        ]
    );
    assert_eq!(
        messages(g),
        [
            "data0.1 isn't a constructor of data0",
            "the case on %0 has no default arm and doesn't cover every value",
        ]
    );
}

#[test]
#[should_panic(expected = "the function has 1 parameters, not 2")]
fn params_panics_on_the_wrong_count() {
    let m = module("", vec![I32], vec![]);
    let f = FuncBuilder::new(&m, vec![I32], vec![]);
    let [_, _] = f.params();
}

/// A data type with a mutability parameter, built with its arguments, and
/// rebuilt as `imm` around a clone of its array.
#[test]
fn mutability_arguments() {
    let decls = "
data data0['m1] {
  ('m1 [i32])
}
";
    let boxed = CtorId {
        data: DataId(0),
        index: 0,
    };
    let m = module(decls, vec![], vec![I32]);
    let mut f = FuncBuilder::new(&m, vec![], vec![I32]);
    let xs = f.array_fill(I32, Mutability::Mut, 1i32, 0i32);
    let b = f.new_object_with(boxed, Mutability::Mut, vec![Mutability::Mut], [xs]);
    let inner = f.get(boxed, 0, b);
    let shared = f.array_clone(Mutability::Imm, inner);
    let held = f.new_object_with(boxed, Mutability::Imm, vec![Mutability::Imm], [shared]);
    let back = f.get(boxed, 0, held);
    let x = f.array_get(back, 0i32);
    f.ret([x]);
    assert_eq!(
        text(f),
        "fn fn0() -> (i32) {
  let %0: mut [i32] = array_fill(1i32, 0i32)
  let %1: mut data0[mut] = new data0.0(%0)
  let %2: mut [i32] = get data0.0.0(%1)
  let %3: imm [i32] = array_clone(%2)
  let %4: imm data0[imm] = new data0.0(%3)
  let %5: imm [i32] = get data0.0.0(%4)
  let %6: i32 = array_get(%5, 0i32)
  jump ret(%6)
}"
    );
}
