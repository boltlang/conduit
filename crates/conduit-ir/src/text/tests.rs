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

data data13['m1, 'm2] {
  ('m1 [i32], 'm2 data10, inherit data13['m2, 'm1])
}

#[link(wasm, module = "env", name = "print \"it\"")]
#[abi(flat)]
extern extern1(%x: i32) -> ()

#[link(wasm, module = "env", name = "fill")]
#[abi(flat)]
extern extern2(#[addr] %buf: mut [i8], i32) -> ()

const const40: imm [i8] = b"hi \"\\\n\t\r\0\x01\xff é"

const const41: imm data11 = data11.1(1i32, data11.0())

const const42: imm [imm data11] = [const41, data11.0()]

const const43: imm [f64] = []

const const44: imm [i16] = [1i16, -1i16]

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
  let %1: imm fn() -> () does effect20 = closure fn7()
  let = handle handler30(%0, %1)
  let %2: i32 = ref_get(%0)
  jump ret(%2)
}

fn fn7() -> () does effect20 {
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
  let %1: imm data10 = new data10.0(1i32, 2i32)
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
  let %13: imm [i64] = array_clone(%5)
  let %14: imm [f64] = array_freeze(%4)
  let %15: ro [i8] = const const40()
  let %16: mut [i64] = array_alloc(2i32)
  let = array_copy(%16, 0i32, %5, 1i32, 2i32)
  jump ret(%1)
}

fn fn9(%0: mut data13[imm, mut]) -> (ro data13[imm, mut]) {
  jump ret(%0)
}

fn fn10['m1, 'm2](%0: 'm2 [i32], %1: 'm1 data13['m1, 'm2]) -> ('m1 data13['m2, 'm1]) where 'm2: 'm1, 'm1: mut {
  let %2: 'm1 [i32] = call fn11['m1](5i32)
  let %3: 'm1 data10 = new data10.0(1i32, 2i32)
  let %4: 'm1 data13['m2, 'm1] = new data13.0(%0, %3, %1)
  let %5: 'm1 fn() -> ('m2 [i32]) = closure fn11['m2](3i32)
  let %6: mut [i32] = call fn11[mut](1i32)
  jump ret(%4)
}

fn fn11['m1](%0: i32) -> ('m1 [i32]) vars(%1: i32) {
  let %2: 'm1 [i32] = array_fill(%0, 0i32)
  jump ret(%2)
}

fn fn12(%0: i32) -> () {
  case %0 {
    0 => trap
    _ => unreachable
  }
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
    assert_eq!(
        m.externs[&ExternId(1)].link,
        Link::Wasm {
            module: "env".into(),
            name: "print \"it\"".into()
        }
    );
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
        "2:8: `j0` isn't a label: they start at `j1`"
    );
    assert_eq!(
        error("fn fn1() -> (mut i32) { jump ret() }"),
        "1:18: expected an ID like `data1`, or a name, found `i32`"
    );
    assert_eq!(
        error("fn fn1() -> () {"),
        "1:17: expected `let`, `join`, `jump`, `case`, `unreachable` or `trap`, found the end of the text"
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

/// Every kind of mistake the parser reports, with where it is.
#[test]
fn parse_errors() {
    let cases: &[(&str, &str)] = &[
        (
            "handler handler1 for effect1, effect2 env() ops(fn1) ret fn2",
            "1:1: a handler for 2 effects has clauses for 1, apart by `;`",
        ),
        (
            "handler handler1 for effect1 env() ops(fn1 fn2) ret fn2",
            "1:44: expected `,`, `;` or `)`, found `fn2`",
        ),
        (
            "data data1['m1, 'm1] { }",
            "1:17: `'m1` is declared more than once",
        ),
        (
            "data data1[m1] { }",
            "1:12: expected a parameter like `'m1` or `E1`, found `m1`",
        ),
        (
            "fn fn1(%0: 'm1 [i32]) -> () { jump ret() }",
            "1:12: unknown mutability parameter `'m1`: declare it in brackets after the data type's or function's ID",
        ),
        (
            "fn fn1['m1, 'm1]() -> () { jump ret() }",
            "1:13: `'m1` is declared more than once",
        ),
        (
            "fn fn1['m1]() -> () where 'm1 imm { jump ret() }",
            "1:31: expected `:`, found `imm`",
        ),
        (
            "fn fn1() -> () { let = call fn2[ro ro]() jump ret() }",
            "1:36: expected `,`, found `ro`",
        ),
        (
            "data data1['m1] { ('m2 [i32]) }",
            "1:20: unknown mutability parameter `'m2`: declare it in brackets after the data type's or function's ID",
        ),
        (
            "frob",
            "1:1: expected `data`, `extern`, `const`, `effect`, `handler` or `fn`, found `frob`",
        ),
        (
            "fn",
            "1:3: expected an ID like `fn1`, or a name, found the end of the text",
        ),
        ("fn fn1", "1:7: expected `(`, found the end of the text"),
        (
            "fn ret() -> () { jump ret() }",
            "1:4: expected an ID like `fn1`, or a name, found `ret`",
        ),
        (
            "fn fn1() -> () { jump ret() }\ndata data1 { }\ndata data1 { }",
            "3:1: data1 is defined more than once",
        ),
        (
            "#[link(wasm, module = \"a\", name = \"b\")]\nextern extern1() -> ()\n#[link(wasm, module = \"a\", name = \"b\")]\nextern extern1() -> ()",
            "4:1: extern1 is defined more than once",
        ),
        (
            "effect effect1 { }\neffect effect1 { }",
            "2:1: effect1 is defined more than once",
        ),
        (
            "handler handler1 for effect1 env() ops() ret fn1\nhandler handler1 for effect1 env() ops() ret fn1",
            "2:1: handler1 is defined more than once",
        ),
        (
            "#[link(wasm, module = a)]\nextern extern1() -> ()",
            "1:23: expected a string, found `a`",
        ),
        (
            "#[link(wasm, module = \"a\", name = \"b\")]\nextern extern1(wat) -> ()",
            "2:16: expected a type, found `wat`",
        ),
        (
            "data data1 { (imm) }",
            "1:18: expected an ID like `data1`, or a name, found `)`",
        ),
        (
            "data data1 { (frob data2) }",
            "1:15: expected a type, found `frob`",
        ),
        (
            "fn fn1(0: i32) -> () { jump ret() }",
            "1:8: expected a variable, found `0`",
        ),
        (
            "fn fn1() -> () { join ret() = jump ret() in jump ret() }",
            "1:23: `ret` can't be defined",
        ),
        (
            "fn fn1(%0: i32) -> () { case %0 { 99999999999999999999 => jump ret() _ => jump ret() } }",
            "1:35: `99999999999999999999` isn't a tag",
        ),
        (
            "fn fn1(%0: i32) -> () { case %0 { _ => jump ret() _ => jump ret() } }",
            "1:51: a case has one `_` arm at most",
        ),
        (
            "fn fn1() -> () { frob }",
            "1:18: expected `let`, `join`, `jump`, `case`, `unreachable` or `trap`, found `frob`",
        ),
        (
            "fn fn1() -> () { jump ret(=) }",
            "1:27: expected a variable or a literal, found `=`",
        ),
        (
            "fn fn1() -> () { jump ret(f32_bits(99999999999)) }",
            "1:27: bad bits for `f32_bits`",
        ),
        (
            "fn fn1() -> () { jump ret(f32_bits(x)) }",
            "1:27: bad bits for `f32_bits`",
        ),
        (
            "fn fn1() -> () { jump ret(frob) }",
            "1:27: expected a variable or a literal, found `frob`",
        ),
        (
            "fn fn1() -> () { let %0: i32 = add.x(1i32, 1i32) jump ret() }",
            "1:36: expected `s` or `u`, found `x`",
        ),
        (
            "fn fn1() -> () { let %0: i32 = add.s(1i32, 1i32) jump ret() }",
            "1:32: `add` has no signed or unsigned form",
        ),
        (
            "fn fn1() -> () { let %0: f32 = reinterpret.s[f32](1i32) jump ret() }",
            "1:32: `reinterpret` has no signed or unsigned form",
        ),
        (
            "fn fn1() -> () { let %0: f32 = convert[f32](1i32) jump ret() }",
            "1:32: `convert` needs `.s` or `.u`, like `convert.s`",
        ),
        (
            "fn fn1() -> () { let %0: i32 = get data1.x.0(1i32) jump ret() }",
            "1:42: expected a number, found `x`",
        ),
        (
            "fn fn1() -> () { let %0: i32 = add(1i32, 1i32) @foo jump ret() }",
            "1:49: expected an ID like `src1`, found `foo`",
        ),
        (
            "fn fn1() -> () { jump ret(1i32 }",
            "1:32: expected `,` or `)`, found `}`",
        ),
        (
            "fn fn1() -> () labels x { jump ret() }",
            "1:23: expected a number, found `x`",
        ),
        (
            "#[link(wasm, module = \"a\\q\")]",
            "1:23: unknown escape in a string",
        ),
        ("#[link(wasm, module = \"a", "1:23: unterminated string"),
        ("#[link(wasm, module = \"a\\", "1:23: unterminated string"),
        (
            "extern extern1() -> ()",
            "1:1: an extern needs a `#[link(..)]`, like `#[link(emu)]`",
        ),
        (
            "#[link(wasm, module = \"a\")]\n#[link(wasm, module = \"a\")]\nextern f() -> ()",
            "2:3: an extern has one `link`",
        ),
        (
            "#[link(native, module = \"a\")]\nextern f() -> ()",
            "1:3: `native` isn't a kind of link: `wasm`, `emu` and `c` are",
        ),
        (
            "#[link(emu, module = \"a\")]\nextern f() -> ()",
            "1:3: an emu `link` takes `name` once each, not `module`",
        ),
        (
            "#[link(c, name = \"a\")]\nextern f() -> ()",
            "1:3: a c `link` takes `symbol` and `lib` once each, not `name`",
        ),
        (
            "#[link(emu)]\nextern extern1() -> ()",
            "1:3: an emu `link` of an extern without a name needs a `name`",
        ),
        (
            "#[link(module = \"a\")]\nextern f() -> ()",
            "1:3: a `link` starts with its kind, like `wasm`",
        ),
        (
            "#[link(wasm, symbol = \"a\")]\nextern f() -> ()",
            "1:3: a wasm `link` takes `module` and `name` once each, not `symbol`",
        ),
        (
            "#[link(wasm, name = \"a\")]\nextern f() -> ()",
            "1:3: a wasm `link` needs a `module`",
        ),
        (
            "#[link(wasm, module = \"a\")]\nextern extern1() -> ()",
            "1:3: a wasm `link` of an extern without a name needs a `name`",
        ),
        (
            "#[inline]\nextern f() -> ()",
            "1:3: `inline` isn't an attribute of an extern: only `link` and `abi` are",
        ),
        (
            "#[link(wasm, module = \"a\")]\nfn f() -> () { jump ret() }",
            "1:3: `link` isn't an attribute of a function: only `export` is",
        ),
        (
            "#[export(wasm)]\ndata d { () }",
            "1:3: only an extern or a function takes attributes, so far",
        ),
        (
            "#[export(wasm)]\n#[export(emu)]\nfn f() -> () { jump ret() }",
            "2:3: a function has one `export`",
        ),
        (
            "#[export(emu, symbol = \"f\")]\nfn f() -> () { jump ret() }",
            "1:3: an emu `export` takes `name` once each, not `symbol`",
        ),
        (
            "#[export(js)]\nfn f() -> () { jump ret() }",
            "1:3: `js` isn't a kind of export: `wasm`, `emu` and `c` are",
        ),
        (
            "#[export(wasm)]\nfn fn1() -> () { jump ret() }",
            "1:3: a wasm `export` of a function without a name needs a `name`",
        ),
        (
            "#[link(wasm, module = \"a\")]\n#[abi(c)]\nextern f() -> ()",
            "2:3: `c` isn't an ABI: `flat` and `conduit` are",
        ),
        (
            "#[abi(flat)]\n#[abi(flat)]\n#[link(wasm, module = \"a\")]\nextern f() -> ()",
            "2:3: an extern has one `abi`",
        ),
        (
            "#[abi]\n#[link(wasm, module = \"a\")]\nextern f() -> ()",
            "1:3: an `abi` is one word, like `abi(flat)`",
        ),
        (
            "#[abi(flat, conduit)]\n#[link(wasm, module = \"a\")]\nextern f() -> ()",
            "1:3: an `abi` is one word, like `abi(flat)`",
        ),
        (
            "#[link(wasm, module = \"a\")]\nextern f(#[addr(x)] i32) -> ()",
            "2:12: `addr` takes no arguments",
        ),
        (
            "#[link(wasm, module = \"a\")]\nextern f(#[cold] i32) -> ()",
            "2:12: `cold` isn't an attribute of a parameter: only `addr` is",
        ),
        (
            "fn fn1(% : i32) -> () { jump ret() }",
            "1:8: expected a variable after `%`, like `%3` or `%acc`",
        ),
        (
            "fn fn1() -> () { jump ret() } $",
            "1:31: unexpected character `$`",
        ),
        (
            "fn fn1() -> (i32) { jump ret(1x32) }",
            "1:30: `1` isn't a literal: it needs a type, like `1i32`",
        ),
        (
            "fn fn1(%0: mut data1[frob]) -> () { jump ret() }",
            "1:22: `frob` isn't defined",
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(error(src), *expected, "for {src:?}");
    }
}

/// A byte string is an array of `i8` literals, and an array of them
/// prints as a byte string.
#[test]
fn byte_strings() {
    let m = parse("const c: imm [i8] = b\"a\\xff\\n\"").unwrap();
    let lits =
        |ns: &[i8]| ConstValue::Array(ns.iter().map(|&n| ConstValue::Lit(Lit::I8(n))).collect());
    assert_eq!(m.consts[&ConstId(0)].value, lits(&[97, -1, 10]));
    let m = parse("const c: imm [i8] = [104i8, 105i8, 255i8]").unwrap();
    assert_eq!(print(&m), "const c: imm [i8] = b\"hi\\xff\"\n");
    // Valid UTF-8 prints as its characters, but not control characters.
    let m = parse("const c: imm [i8] = b\"\\xc3\\xa9\\xc2\\x85\"").unwrap();
    assert_eq!(print(&m), "const c: imm [i8] = b\"é\\xc2\\x85\"\n");
    for (src, message) in [
        (
            "const c: imm [i8] = b\"ab",
            "1:21: unterminated byte string",
        ),
        (
            "const c: imm [i8] = b\"\\q\"",
            "1:21: unknown escape in a byte string",
        ),
        (
            "const c: imm [i8] = b\"\\x+f\"",
            "1:21: `\\x` needs two hex digits, like `\\x7f`",
        ),
        (
            "const c: imm [i8] = b\"\\x4\"",
            "1:21: `\\x` needs two hex digits, like `\\x7f`",
        ),
    ] {
        assert_eq!(error(src), message, "for {src:?}");
    }
}

/// Escapes in strings print back as they were written.
#[test]
fn string_escapes_round_trip() {
    let src =
        "#[link(wasm, module = \"a\\\\b\\n\\t\\\"\", name = \"x\")]\nextern extern1() -> ()\n";
    let m = parse(src).unwrap();
    let Link::Wasm { module, .. } = &m.externs[&ExternId(1)].link else {
        panic!("not a wasm link");
    };
    assert_eq!(&**module, "a\\b\n\t\"");
    assert_eq!(print(&m), src);
    assert_eq!(m.to_string(), src);
}

/// A variable without a type, which only broken IR has, prints as
/// `%n: ?`, so that it doesn't read back as something valid.
#[test]
fn broken_ir_prints_without_reading_back() {
    let mut f = Func::new(vec![], vec![]);
    f.body = Block {
        lets: vec![Let {
            binds: vec![Var(3)],
            op: Op::Prim(Prim::Add),
            args: vec![],
            source: None,
        }],
        tail: Tail::Unreachable,
    };
    let text = print_func(FuncId(1), &f);
    assert!(text.contains("%3: ?"), "{text}");
    assert!(parse(&text).is_err());
}

/// The same constructs as [`CANONICAL`], with names, in the form `print`
/// writes them: names alone where they read back to the same number, and
/// with it where they don't.
const NAMED: &str = r#"data Pair {
  (i32, i32)
}

data List {
  ()
  (i32, inherit List)
}

#[link(wasm, module = "env")]
extern print(i32) -> ()

effect Yield {
  (i32) -> ()
}

handler adder for Yield env(ref i32) ops(add_yield) ret done

fn add_yield(%acc: ref i32, %x: i32, %k: cont() -> ()) -> () {
  let %old: i32 = ref_get(%acc)
  let %new: i32 = add(%old, %x)
  let = ref_set(%acc, %new)
  let = resume(%k)
  jump ret()
}

fn done(%acc: ref i32) -> () {
  jump ret()
}

fn sum(%n: i32) -> (i32) {
  let %acc: ref i32 = ref_new(0i32)
  let %body: imm fn() -> () does Yield = closure count(%n)
  let = handle adder(%acc, %body)
  let %total: i32 = ref_get(%acc)
  let = call_extern print(%total)
  jump ret(%total)
}

fn count(%n: i32) -> () does Yield {
  join rec loop(%i: i32) =
    let %more: i32 = lt.s(%i, %n)
    case %more {
      1 =>
        let = perform Yield.0(%i)
        let %next: i32 = add(%i, 1i32)
        jump loop(%next)
      _ => jump ret()
    }
  in jump loop(0i32)
}

fn length(%xs: imm List) -> (i32) {
  case %xs {
    List.0 => jump ret(0i32)
    List.1 =>
      let %rest: imm List = get List.1.1(%xs)
      let %n: i32 = call length(%rest)
      let %m: i32 = add(%n, 1i32)
      jump ret(%m)
  }
}
"#;

#[test]
fn names_round_trip() {
    let m = parse(NAMED).unwrap();
    assert_eq!(print(&m), NAMED);
    verify_module(&m).unwrap();
    // Names are numbered in order, from 0: `sum` is the third function.
    let sum = &m.funcs[&FuncId(2)];
    assert_eq!(sum.debug.name.as_deref(), Some("sum"));
    assert_eq!(sum.debug.vars[&Var(0)].as_ref(), "n");
    assert_eq!(
        m.handlers[&HandlerId(0)].debug_name.as_deref(),
        Some("adder")
    );
    let count = &m.funcs[&FuncId(3)];
    assert_eq!(count.debug.labels[&Label(1)].as_ref(), "loop");
}

#[test]
fn names_that_dont_give_their_number_are_written_with_it() {
    // Two functions share a name, as instances of a generic function do,
    // and a numbered one comes before a named one that isn't number 0.
    let src = "fn map#3() -> () { jump ret() }

fn map#7() -> () {
  let = call map#3()
  jump ret()
}

fn fn1() -> () { jump ret() }
";
    let m = parse(src).unwrap();
    assert_eq!(m.funcs[&FuncId(7)].debug.name.as_deref(), Some("map"));
    let printed = print(&m);
    assert!(
        printed.contains("fn map#3()") && printed.contains("call map#3()"),
        "{printed}"
    );
    assert_eq!(print(&parse(&printed).unwrap()), printed);

    // A name alone where it gives its number, and with it where it doesn't.
    let mut m = Module::default();
    for (id, name) in [(0, "zero"), (5, "five"), (1, "one")] {
        let mut f = Func::new(vec![], vec![]);
        f.debug.name = Some(name.into());
        f.body = Tail::Jump {
            target: Label::RET,
            args: vec![],
            source: None,
        }
        .into();
        m.funcs.insert(FuncId(id), f);
    }
    let printed = print(&m);
    assert!(printed.contains("fn zero()"), "{printed}");
    assert!(printed.contains("fn one()"), "{printed}");
    assert!(printed.contains("fn five#5()"), "{printed}");
    assert_eq!(parse(&printed).unwrap(), m);
}

#[test]
fn variables_with_the_same_name_are_written_with_their_number() {
    // As after inlining a function twice.
    let mut f = Func::new(vec![Ty::I32], vec![Ty::I32]);
    let x = f.params[0];
    let y = f.new_var(Ty::I32);
    let z = f.new_var(Ty::I32);
    for v in [x, y, z] {
        f.debug.vars.insert(v, "x".into());
    }
    f.body = Block {
        lets: vec![
            Let {
                binds: vec![y],
                op: Op::Prim(Prim::Add),
                args: vec![Atom::Var(x), Atom::Var(x)],
                source: None,
            },
            Let {
                binds: vec![z],
                op: Op::Prim(Prim::Add),
                args: vec![Atom::Var(y), Atom::Var(y)],
                source: None,
            },
        ],
        tail: Tail::Jump {
            target: Label::RET,
            args: vec![Atom::Var(z)],
            source: None,
        },
    };
    let mut m = Module::default();
    m.funcs.insert(FuncId(0), f);
    let printed = print(&m);
    assert!(
        printed.contains("let %x#1: i32 = add(%x#0, %x#0)"),
        "{printed}"
    );
    assert_eq!(parse(&printed).unwrap(), m);
}

#[test]
fn name_errors() {
    for (src, message) in [
        (
            "fn a() -> () { jump ret() }\nfn a() -> () { jump ret() }",
            "1:4: `a` is defined more than once: give each a number, like `a#1`",
        ),
        (
            "fn a() -> () {\n  let = call b()\n  jump ret()\n}",
            "2:14: `b` isn't defined",
        ),
        (
            "fn a#1() -> () { jump ret() }\nfn a#2() -> () {\n  let = call a()\n  jump ret()\n}",
            "3:14: `a` is the name of more than one: say which, like `a#1`",
        ),
    ] {
        assert_eq!(error(src), message, "for {src:?}");
    }
}

#[test]
fn errors_name_their_item() {
    let m = parse("fn main() -> (i64) { jump ret(1i32) }").unwrap();
    let errors = verify_module(&m).unwrap_err();
    assert_eq!(
        errors[0].named(&m),
        "main (fn0): jump to ret: value 0 has type i32, but should have i64"
    );
    assert_eq!(
        errors[0].to_string(),
        "fn0: jump to ret: value 0 has type i32, but should have i64"
    );
}

/// An effect named like an effect parameter is written by its number, so
/// that `E1` in a row always means the parameter.
#[test]
fn effects_named_like_parameters() {
    let mut m = Module::default();
    m.effects.insert(
        EffectId(3),
        EffectDecl {
            ops: vec![],
            debug_name: Some("E1".into()),
        },
    );
    let mut f = Func::new(vec![], vec![]);
    f.effect_params = 1;
    f.row = Row::with_params([EffectId(3)], [1]);
    f.body = Tail::Jump {
        target: Label::RET,
        args: vec![],
        source: None,
    }
    .into();
    m.funcs.insert(FuncId(0), f);
    let text = print(&m);
    assert!(
        text.contains("fn fn0[E1]() -> () does effect3 + E1 {"),
        "{text}"
    );
    // The name can't be written, so it's lost, as for any such name.
    m.effects.get_mut(&EffectId(3)).unwrap().debug_name = None;
    assert_eq!(parse(&text).unwrap(), m);
}

/// An extern whose name can't be written as one is written by number, so
/// its link says its name.
#[test]
fn unwritable_extern_names_round_trip() {
    for name in ["my-fn", "ret"] {
        let mut m = crate::Module::default();
        let link = Link::Wasm {
            module: "env".into(),
            name: name.into(),
        };
        let mut e = crate::Extern::new(link.clone(), crate::Sig::new(vec![], vec![]));
        e.debug_name = Some(name.into());
        m.externs.insert(ExternId(1), e);
        let text = print(&m);
        let back = parse(&text).unwrap();
        assert_eq!(back.externs[&ExternId(1)].link, link, "{text}");
        assert_eq!(print(&back), text);
    }
}

/// Each kind of export, with its name or symbol the function's own and
/// not.
#[test]
fn exports_round_trip() {
    let src = "#[export(wasm)]
fn main() -> () {
  jump ret()
}

#[export(wasm, name = \"_start\")]
fn start() -> () {
  jump ret()
}

#[export(emu)]
fn run() -> () {
  jump ret()
}

#[export(c, symbol = \"lib_init\")]
fn init() -> () {
  jump ret()
}
";
    let m = parse(src).unwrap();
    let exports: Vec<&Export> = m.exports.values().collect();
    assert_eq!(
        exports,
        [
            &Export::Wasm {
                name: "main".into()
            },
            &Export::Wasm {
                name: "_start".into()
            },
            &Export::Emu { name: "run".into() },
            &Export::C {
                symbol: "lib_init".into()
            },
        ]
    );
    assert_eq!(print(&m), src);
}

/// Each kind of link, with its name or symbol the extern's own and not.
#[test]
fn links_round_trip() {
    let src = "#[link(emu)]
extern double(i32) -> (i32)

#[link(emu, name = \"twice\")]
extern times2(i32) -> (i32)

#[link(c, lib = \"c\")]
extern write(i32, imm [i8]) -> (i64)

#[link(c, symbol = \"puts\")]
extern put(imm [i8]) -> (i32)
";
    let m = parse(src).unwrap();
    let links: Vec<&Link> = m.externs.values().map(|e| &e.link).collect();
    assert_eq!(
        links,
        [
            &Link::Emu {
                name: "double".into()
            },
            &Link::Emu {
                name: "twice".into()
            },
            &Link::C {
                symbol: "write".into(),
                lib: Some("c".into())
            },
            &Link::C {
                symbol: "puts".into(),
                lib: None
            },
        ]
    );
    assert_eq!(print(&m), src);
}

/// Errors of parameters and labels that the others don't reach.
#[test]
fn errors_of_params_and_labels() {
    let handler = "effect e { () -> () }
fn c(%k: cont() -> ()) -> () { jump ret() }
fn r() -> () { jump ret() }
handler h['m1] for e env() ops(c) ret r";
    assert_eq!(
        error(handler),
        "4:1: a handler has no mutability parameters, but 'm1 is one"
    );
    assert_eq!(
        error("fn f[E1, 'm1]() -> () { jump ret() }"),
        "1:10: mutability parameter `'m1` comes after an effect parameter"
    );
    assert_eq!(
        error("fn f() -> () {\n  jump rec()\n}"),
        "2:8: expected a label like `j1`, `loop` or `ret`, found `rec`"
    );
}

/// `find_func`, which tests use to name what they call: by number, by
/// name, by both, and what's wrong otherwise.
#[test]
fn finding_functions() {
    let m = parse(
        "fn sum#1() -> () { jump ret() }
fn sum#2() -> () { jump ret() }
fn product#3() -> () { jump ret() }",
    )
    .unwrap();
    assert_eq!(crate::text::find_func(&m, "fn3"), Ok(FuncId(3)));
    assert_eq!(crate::text::find_func(&m, "product"), Ok(FuncId(3)));
    assert_eq!(crate::text::find_func(&m, "sum#2"), Ok(FuncId(2)));
    assert_eq!(
        crate::text::find_func(&m, "sum"),
        Err("more than one function is `sum`: say which, like `sum#1`".into())
    );
    assert_eq!(
        crate::text::find_func(&m, "fn9"),
        Err("there's no function `fn9`".into())
    );
    assert_eq!(
        crate::text::find_func(&m, "quotient"),
        Err("there's no function `quotient`".into())
    );
    assert_eq!(
        crate::text::find_func(&m, "%1"),
        Err("`%1` isn't a function: expected `fn1` or a name".into())
    );
}

/// `throw` is written first in a row, and is built in, so no effect can be
/// named that, and one whose name is `throw` is written by its number.
#[test]
fn throw_in_rows() {
    let text = "effect Log {
  (i32) -> ()
}

fn f(%f: imm fn() -> () does throw + Log) -> () does throw + Log {
  let = apply(%f)
  jump ret()
}
";
    let m = parse(text).unwrap();
    assert_eq!(print(&m), text);
    assert!(m.funcs[&FuncId(0)].row.throws());
    let err = parse("effect throw {\n}\n").unwrap_err();
    assert!(err.to_string().contains("`throw` is built in"), "{err}");
    let mut m = parse("effect e {\n}\n").unwrap();
    m.effects.get_mut(&EffectId(0)).unwrap().debug_name = Some("throw".into());
    assert!(!print(&m).contains("effect throw"), "{}", print(&m));
}

/// `throw` is an effect everywhere one can be named, whose declaration
/// the module gets without writing it, and which must be the built-in
/// one.
#[test]
fn throw_as_an_effect() {
    let text = "handler catch for throw env() ops(caught) ret fine

fn caught(%e: nullable host, %k: cont() -> ()) -> () {
  jump ret()
}

fn fine() -> () {
  jump ret()
}

fn raises(%e: nullable host) -> () does throw {
  let = perform throw.0(%e)
  jump ret()
}
";
    let m = parse(text).unwrap();
    assert_eq!(print(&m), text);
    assert_eq!(m.effects[&EffectId::THROW], EffectDecl::throw());
    verify_module(&m).unwrap();
    let mut wrong = m.clone();
    wrong.effects.get_mut(&EffectId::THROW).unwrap().ops[0].params = vec![Ty::I32];
    let errors = verify_module(&wrong).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("throw is built in")),
        "{errors:?}"
    );
    let mut undeclared = m;
    undeclared.effects.clear();
    assert!(verify_module(&undeclared).is_err());
}
