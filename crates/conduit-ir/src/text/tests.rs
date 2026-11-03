use super::*;
use crate::verify::verify_module;
use crate::*;

/// Uses every construct of the format, in the form `print` writes it.
const CANONICAL: &str = r#"data data10 {
  (i32, i32)
}

data data11 {
  ()
  (i32, inherit data11)
}

data data12 {
}

extern extern1 "env" "print \"it\"" (i32) -> ()

effect effect20 {
  (i32) -> ()
}

handler handler30 for effect20 env(ref i32) ops(fn4) ret fn5

fn fn1(%0: i32) -> (i32) {
  join rec j1(%1: i32, %2: i32) =
    let %3: i32 = ge.s(%1, %0)
    case %3 {
      1 => jump ret(%2)
      _ =>
        let %4: i32 = add(%1, 1i32)
        let %5: i32 = add(%2, %1) @src7
        jump j1(%4, %5) @src8
    }
  in jump j1(0i32, 0i32)
}

fn fn2(%0: imm data11, %1: i32) -> (i32) {
  case %0 {
    data11.1 =>
      let %2: i32 = get data11.1.0(%0)
      jump ret(%2)
    data11.0 => jump ret(%1)
  }
}

fn fn3() -> (f64) labels 3 vars(%1: mut [i32]) {
  let %0: f64 = add(f64_bits(9218868437227405312), 1.0f64)
  jump ret(%0)
}

fn fn4(%0: ref i32, %1: i32, %2: cont() -> ()) -> () {
  let %3: i32 = ref_get(%0)
  let %4: i32 = add(%3, %1)
  let = ref_set(%0, %4)
  let = resume(%2)
  jump ret()
}

fn fn5(%0: ref i32) -> () {
  jump ret()
}

fn fn6() -> (i32) {
  let %0: ref i32 = ref_new(0i32)
  let %1: imm fn() -> () = closure fn7()
  let = handle handler30(%0, %1)
  let %2: i32 = ref_get(%0)
  jump ret(%2)
}

fn fn7() -> () {
  let = perform effect20.0(1i32)
  let = call_extern extern1(5i32)
  join j1() = jump ret()
  and j2(%0: i32) = unreachable
  in
  let %1: i32 = call fn1(3i32)
  jump j1()
}

fn fn8(%0: mut data10) -> (imm data10) {
  let = set data10.0.1(%0, 2i32)
  let %1: imm data10 = copy[imm](%0)
  let %2: imm any = box[i32](1i32)
  let %3: i32 = unbox[i32](%2)
  let %4: mut [f64] = array_new(1.5f64, -0.0f64, 1e300f64)
  let %5: mut [i64] = array_fill(3i32, -9223372036854775808i64)
  let %6: i64 = array_get(%5, 0i32)
  let = array_set(%5, 1i32, %6)
  let %7: i32 = array_len(%4)
  let %8: f32 = reinterpret[f32](%7)
  let %9: f64 = convert.u[f64](%7)
  let %10: mut fn(i32) -> (i32) = closure fn1()
  let %11: i32 = apply(%10, %7)
  let %12: mut data10 = new data10.0(%11, %3)
  jump ret(%1)
}
"#;

#[test]
fn canonical_text_round_trips() {
    let m = parse(CANONICAL).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(print(&m), CANONICAL);
}

#[test]
fn canonical_module_verifies() {
    let m = parse(CANONICAL).unwrap();
    assert_eq!(verify_module(&m), Ok(()));
}

#[test]
fn parsed_module_has_the_right_shape() {
    let m = parse(CANONICAL).unwrap();
    let f = &m.funcs[&FuncId(3)];
    assert_eq!(f.labels, 3);
    assert_eq!(
        f.vars,
        vec![Ty::F64, Ty::Array(Box::new(Ty::I32), Mutability::Mut)]
    );
    assert_eq!(m.data[&DataId(12)].ctors, vec![]);
    assert_eq!(&*m.externs[&ExternId(1)].name, "print \"it\"");
    let Tail::Join {
        rec: true, defs, ..
    } = &m.funcs[&FuncId(1)].body.tail
    else {
        panic!("not a recursive join");
    };
    assert_eq!(defs[0].label, Label(1));
}

#[test]
fn literals_round_trip_exactly() {
    let lits = [
        Lit::I32(i32::MIN),
        Lit::I64(i64::MAX),
        Lit::F32(0x7fc0_0001), // a NaN with a payload
        Lit::F32((-0.0f32).to_bits()),
        Lit::f32(f32::MIN_POSITIVE),
        Lit::f32(f32::NEG_INFINITY),
        Lit::f64(0.1),
        Lit::f64(f64::MAX),
        Lit::F64(f64::NAN.to_bits() | 5),
    ];
    let mut f = Func::new(vec![], vec![]);
    let mut body = Block::from(Tail::Jump {
        target: Label::RET,
        args: vec![],
        source: None,
    });
    for lit in lits.iter().rev() {
        let v = f.new_var(lit.ty());
        body = (body).with_let(Let {
            binds: vec![v],
            op: Op::Prim(Prim::Add),
            args: vec![Atom::Lit(*lit), Atom::Lit(*lit)],
            source: None,
        });
    }
    f.body = body;
    let mut m = Module::default();
    m.funcs.insert(FuncId(1), f);
    let text = print(&m);
    assert_eq!(
        parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}")),
        m,
        "{text}"
    );
}

#[test]
fn comments_and_layout_are_ignored() {
    let src = "// A function.\nfn fn1(%0: i32) -> (i32) { let %1: i32 = add(%0, %0) // twice\n jump ret(%1) }";
    let m = parse(src).unwrap();
    assert_eq!(
        print(&m),
        "fn fn1(%0: i32) -> (i32) {\n  let %1: i32 = add(%0, %0)\n  jump ret(%1)\n}\n"
    );
}

fn error(src: &str) -> String {
    parse(src).unwrap_err().to_string()
}

#[test]
fn errors_say_where() {
    assert_eq!(
        error("fn fn1() -> () {\n  let %0: i32 = frob(1i32)\n  jump ret()\n}"),
        "2:17: unknown op `frob`"
    );
    assert_eq!(
        error("fn fn1() -> () {\n  let %0: i32 = add(1, 2i32)\n  jump ret()\n}"),
        "2:21: `1` isn't a literal: it needs a type, like `1i32`"
    );
    assert_eq!(
        error("fn fn1() -> () {\n  let %0: i32 = div(1i32, 2i32)\n  jump ret()\n}"),
        "2:17: `div` needs `.s` or `.u`, like `div.s`"
    );
    assert_eq!(
        error("fn fn1(%0: i32) -> () {\n  join j1(%0: i64) = jump ret()\n  in jump ret()\n}"),
        "2:11: %0 has type i32 elsewhere, not i64"
    );
    assert_eq!(
        error("fn fn1() -> () { jump ret() }\nfn fn1() -> () { jump ret() }"),
        "2:1: fn1 is defined more than once"
    );
    assert_eq!(
        error("fn fn1() -> () {\n  jump j0()\n}"),
        "2:8: expected a label like `j1` or `ret`, found `j0`"
    );
    assert_eq!(
        error("fn fn1() -> (mut i32) { jump ret() }"),
        "1:18: expected an ID like `data1`, found `i32`"
    );
    assert_eq!(
        error("fn fn1() -> () {"),
        "1:17: expected `let`, `join`, `jump`, `case` or `unreachable`, found the end of the text"
    );
}

#[test]
fn every_variable_needs_a_type() {
    // %1 is between bound variables, but has no type.
    let src = "fn fn1(%0: i32, %2: i32) -> () {\n  jump ret()\n}";
    assert_eq!(error(src), "3:2: %1 has no type: give it one in `vars(..)`");
}

#[test]
fn long_functions_dont_need_a_deep_stack() {
    let n = 50_000;
    let mut src = String::from("fn fn1() -> (i32) {\n  let %0: i32 = add(0i32, 1i32)\n");
    for i in 1..n {
        src.push_str(&format!("  let %{i}: i32 = add(%{}, 1i32)\n", i - 1));
    }
    src.push_str(&format!("  jump ret(%{})\n}}\n", n - 1));
    let m = parse(&src).unwrap();
    assert_eq!(print(&m), src);
    // A block's `let`s are a list, so cloning, comparing and dropping a
    // long function don't recurse either.
    assert_eq!(m.clone(), m);
    drop(m);
}
