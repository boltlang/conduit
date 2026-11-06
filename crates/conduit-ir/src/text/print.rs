use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use super::names::spellings;
use crate::*;

/// How each item is written, of each kind: see [`super::names`].
#[derive(Default)]
struct Items {
    data: BTreeMap<u64, String>,
    externs: BTreeMap<u64, String>,
    effects: BTreeMap<u64, String>,
    handlers: BTreeMap<u64, String>,
    funcs: BTreeMap<u64, String>,
    consts: BTreeMap<u64, String>,
}

fn named<'a, K: Copy, V>(
    items: &'a BTreeMap<K, V>,
    id: impl Fn(K) -> u64,
    name: impl Fn(&'a V) -> &'a Option<Box<str>>,
) -> Vec<(u64, Option<&'a str>)> {
    items
        .iter()
        .map(|(&k, v)| (id(k), name(v).as_deref()))
        .collect()
}

impl Items {
    fn new(m: &Module) -> Items {
        Items {
            data: spellings("data", 0, &named(&m.data, |d| d.0, |d| &d.debug_name)),
            externs: spellings("extern", 0, &named(&m.externs, |e| e.0, |e| &e.debug_name)),
            effects: spellings("effect", 0, &named(&m.effects, |e| e.0, |e| &e.debug_name)),
            handlers: spellings(
                "handler",
                0,
                &named(&m.handlers, |h| h.0, |h| &h.debug_name),
            ),
            funcs: spellings("fn", 0, &named(&m.funcs, |f| f.0, |f| &f.debug.name)),
            consts: spellings("const", 0, &named(&m.consts, |c| c.0, |c| &c.debug_name)),
        }
    }

    fn get(map: &BTreeMap<u64, String>, prefix: &str, id: u64) -> String {
        map.get(&id)
            .cloned()
            .unwrap_or_else(|| format!("{prefix}{id}"))
    }

    fn data(&self, d: DataId) -> String {
        Self::get(&self.data, "data", d.0)
    }

    fn ctor(&self, c: CtorId) -> String {
        format!("{}.{}", self.data(c.data), c.index)
    }

    fn extern_(&self, e: ExternId) -> String {
        Self::get(&self.externs, "extern", e.0)
    }

    fn effect(&self, e: EffectId) -> String {
        Self::get(&self.effects, "effect", e.0)
    }

    fn handler(&self, h: HandlerId) -> String {
        Self::get(&self.handlers, "handler", h.0)
    }

    fn func(&self, f: FuncId) -> String {
        Self::get(&self.funcs, "fn", f.0)
    }

    fn constant(&self, c: ConstId) -> String {
        Self::get(&self.consts, "const", c.0)
    }

    /// A constant's value: `5i32`, `[1i32, 2i32]`, `b"hi"`,
    /// `List.1(1i32, nil)`.
    fn const_value(&self, value: &ConstValue) -> String {
        let all = |parts: &[ConstValue]| {
            parts
                .iter()
                .map(|v| self.const_value(v))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match value {
            ConstValue::Lit(lit) => lit_text(lit),
            ConstValue::Array(elems) => match bytes(elems) {
                Some(bytes) => byte_string(&bytes),
                None => format!("[{}]", all(elems)),
            },
            ConstValue::New(c, fields) => format!("{}({})", self.ctor(*c), all(fields)),
            ConstValue::Const(id) => self.constant(*id),
        }
    }

    fn ty(&self, ty: &Ty) -> String {
        match ty {
            Ty::Data(d, m, args, rows) => format!("{m} {}{}", self.data(*d), self.args(args, rows)),
            Ty::Array(elem, m) => format!("{m} [{}]", self.ty(elem)),
            Ty::Fn(sig, m) => format!("{m} fn{}", self.sig(sig)),
            Ty::Cont(sig) => format!("cont{}", self.sig(sig)),
            Ty::Ref(ty) => format!("ref {}", self.ty(ty)),
            Ty::Reuse(c) => format!("reuse {}", self.ctor(*c)),
            other => other.to_string(),
        }
    }

    fn types(&self, tys: &[Ty]) -> String {
        tys.iter()
            .map(|ty| self.ty(ty))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn sig(&self, sig: &Sig) -> String {
        format!(
            "({}) -> ({}){}",
            self.types(&sig.params),
            self.types(&sig.results),
            self.does(&sig.row)
        )
    }

    /// ` does row`, or nothing for no effects.
    fn does(&self, row: &Row) -> String {
        if row.is_nothing() {
            String::new()
        } else {
            format!(" does {}", self.row(row))
        }
    }

    /// A row: `Effect20 + E1`, `Nothing` or `Any`.
    fn row(&self, row: &Row) -> String {
        if row.is_any() {
            return "Any".to_string();
        }
        if row.is_nothing() {
            return "Nothing".to_string();
        }
        let items: Vec<String> = row
            .effects()
            .iter()
            .map(|&e| self.effect(e))
            .chain(row.params().iter().map(|p| format!("E{p}")))
            .collect();
        items.join(" + ")
    }

    /// Mutability arguments and rows in brackets, like `[imm, Log]`, or
    /// nothing if there are none.
    fn args(&self, margs: &[Mutability], rows: &[Row]) -> String {
        if margs.is_empty() && rows.is_empty() {
            return String::new();
        }
        let all: Vec<String> = margs
            .iter()
            .map(Mutability::to_string)
            .chain(rows.iter().map(|r| self.row(r)))
            .collect();
        format!("[{}]", all.join(", "))
    }

    fn op(&self, op: &Op) -> String {
        match op {
            Op::Call(f, margs, rows) => format!("call {}{}", self.func(*f), self.args(margs, rows)),
            Op::CallExtern(e) => format!("call_extern {}", self.extern_(*e)),
            Op::Closure(f, margs, rows) => {
                format!("closure {}{}", self.func(*f), self.args(margs, rows))
            }
            Op::New(c) => format!("new {}", self.ctor(*c)),
            Op::Reuse(c) => format!("reuse {}", self.ctor(*c)),
            Op::Const(id) => format!("const {}", self.constant(*id)),
            Op::Get(c, i) => format!("get {}.{i}", self.ctor(*c)),
            Op::Set(c, i) => format!("set {}.{i}", self.ctor(*c)),
            Op::Box(ty) => format!("box[{}]", self.ty(ty)),
            Op::Unbox(ty) => format!("unbox[{}]", self.ty(ty)),
            Op::Handle(h, rows) => format!("handle {}{}", self.handler(*h), self.args(&[], rows)),
            Op::Perform(e, i) => format!("perform {}.{i}", self.effect(*e)),
            Op::Discontinue(e, i) => format!("discontinue {}.{i}", self.effect(*e)),
            op => op_name(op),
        }
    }
}

pub(super) fn module(m: &Module) -> String {
    let names = Items::new(m);
    let mut items = Vec::new();
    for (id, decl) in &m.data {
        let name = names.data(*id);
        let params = param_list(decl.params, decl.effect_params);
        let mut s = format!("data {name}{params} {{\n");
        for ctor in &decl.ctors {
            let _ = writeln!(s, "  ({})", names.types(&ctor.fields));
        }
        s.push('}');
        items.push(s);
    }
    for (id, ext) in &m.externs {
        items.push(format!(
            "extern {} {} {} {}",
            names.extern_(*id),
            string(&ext.module),
            string(&ext.name),
            names.sig(&ext.sig)
        ));
    }
    for (id, decl) in &m.consts {
        items.push(format!(
            "const {}: {} = {}",
            names.constant(*id),
            names.ty(&decl.ty),
            names.const_value(&decl.value)
        ));
    }
    for (id, effect) in &m.effects {
        let mut s = format!("effect {} {{\n", names.effect(*id));
        for sig in &effect.ops {
            let _ = writeln!(s, "  {}", names.sig(sig));
        }
        s.push('}');
        items.push(s);
    }
    for (id, h) in &m.handlers {
        let ops = (h.ops.iter())
            .map(|(_, clauses)| {
                let clauses: Vec<String> = clauses.iter().map(|&f| names.func(f)).collect();
                clauses.join(", ")
            })
            .collect::<Vec<_>>()
            .join("; ");
        let effects: Vec<String> = h.effects().map(|e| names.effect(e)).collect();
        items.push(format!(
            "handler {}{} for {}{} env({}) ops({ops}) ret {}",
            names.handler(*id),
            param_list(0, h.effect_params),
            effects.join(", "),
            names.does(&h.row),
            names.types(&h.env),
            names.func(h.ret)
        ));
    }
    for (&id, f) in &m.funcs {
        items.push(func_with(&names, id, f));
    }
    let mut out = items.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

pub(super) fn func(id: FuncId, f: &Func) -> String {
    let names = Items {
        funcs: spellings("fn", 0, &[(id.0, f.debug.name.as_deref())]),
        ..Items::default()
    };
    func_with(&names, id, f)
}

/// Marks a variable or label in the text until its spelling is known.
const VAR: (char, char) = ('\u{1}', '\u{2}');
const LABEL: (char, char) = ('\u{3}', '\u{4}');

fn func_with(names: &Items, id: FuncId, f: &Func) -> String {
    let mut p = Printer {
        f,
        names,
        out: String::new(),
    };
    let params = f
        .params
        .iter()
        .map(|&v| p.binder(v))
        .collect::<Vec<_>>()
        .join(", ");
    let mparams = param_list(f.mut_params.count, f.effect_params);
    let _ = write!(
        p.out,
        "fn {}{mparams}({params}) -> ({}){}",
        names.func(id),
        names.types(&f.results),
        names.does(&f.row)
    );
    if !f.mut_params.assumes.is_empty() {
        let assumes: Vec<String> = f
            .mut_params
            .assumes
            .iter()
            .map(|(a, b)| format!("{a}: {b}"))
            .collect();
        let _ = write!(p.out, " where {}", assumes.join(", "));
    }

    let mut bound = BTreeSet::new();
    let mut max_label = 0;
    f.body.walk(|b| {
        for l in &b.lets {
            bound.extend(l.binds.iter().copied());
        }
        if let Tail::Join { defs, .. } = &b.tail {
            for def in defs {
                bound.extend(def.params.iter().copied());
                max_label = max_label.max(def.label.0);
            }
        }
    });
    bound.extend(f.params.iter().copied());
    if f.labels != max_label + 1 {
        let _ = write!(p.out, " labels {}", f.labels);
    }
    let unbound: Vec<String> = (0..f.vars.len() as u32)
        .map(Var)
        .filter(|v| !bound.contains(v))
        .map(|v| p.binder(v))
        .collect();
    if !unbound.is_empty() {
        let _ = write!(p.out, " vars({})", unbound.join(", "));
    }
    p.out.push_str(" {\n");
    p.block(&f.body, 1);
    p.out.push('}');
    spell_locals(&p.out, f)
}

/// `text` with each marked variable and label spelled, as the parser will
/// number them in the order they first appear.
fn spell_locals(text: &str, f: &Func) -> String {
    let marked = |(open, close): (char, char)| {
        let mut order: Vec<u32> = Vec::new();
        let mut rest = text;
        while let Some(at) = rest.find(open) {
            let end = rest[at..].find(close).expect("a closed mark") + at;
            let n: u32 = rest[at + 1..end].parse().expect("a number in a mark");
            if !order.contains(&n) {
                order.push(n);
            }
            rest = &rest[end + 1..];
        }
        order
    };
    let var_order = marked(VAR);
    let label_order = marked(LABEL);
    let vars: Vec<(u64, Option<&str>)> = var_order
        .iter()
        .map(|&n| (n.into(), f.debug.vars.get(&Var(n)).map(|s| &**s)))
        .collect();
    let labels: Vec<(u64, Option<&str>)> = label_order
        .iter()
        .map(|&n| (n.into(), f.debug.labels.get(&Label(n)).map(|s| &**s)))
        .collect();
    let vars = spellings("", 0, &vars);
    let labels = spellings("j", 1, &labels);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        let (close, table, sigil) = if c == VAR.0 {
            (VAR.1, &vars, "%")
        } else if c == LABEL.0 {
            (LABEL.1, &labels, "")
        } else {
            out.push(c);
            continue;
        };
        let end = text[at..].find(close).expect("a closed mark") + at;
        let n: u64 = text[at + 1..end].parse().expect("a number in a mark");
        out.push_str(sigil);
        out.push_str(&table[&n]);
        while chars.next().is_some_and(|(i, _)| i < end) {}
    }
    out
}

struct Printer<'a> {
    f: &'a Func,
    names: &'a Items,
    out: String,
}

impl Printer<'_> {
    fn var(&self, v: Var) -> String {
        format!("{}{}{}", VAR.0, v.0, VAR.1)
    }

    fn label(&self, l: Label) -> String {
        if l == Label::RET {
            "ret".to_string()
        } else {
            format!("{}{}{}", LABEL.0, l.0, LABEL.1)
        }
    }

    fn atom(&self, a: &Atom) -> String {
        match a {
            Atom::Var(v) => self.var(*v),
            Atom::Lit(lit) => lit_text(lit),
        }
    }

    fn atoms(&self, args: &[Atom]) -> String {
        args.iter()
            .map(|a| self.atom(a))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `%3: i32`. A variable without a type, which only broken IR has,
    /// prints as `%3: ?`, which doesn't parse.
    fn binder(&self, v: Var) -> String {
        match self.f.vars.get(v.0 as usize) {
            Some(ty) => format!("{}: {}", self.var(v), self.names.ty(ty)),
            None => format!("{}: ?", self.var(v)),
        }
    }

    fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// Prints `head`, followed by `b` on the same line if it's a single
    /// line, or indented below it otherwise.
    fn nested(&mut self, depth: usize, head: &str, b: &Block) {
        match self.simple(b) {
            Some(text) => self.line(depth, &format!("{head} {text}")),
            None => {
                self.line(depth, head);
                self.block(b, depth + 1);
            }
        }
    }

    /// The text of a block that fits on one line: one without `let`s that
    /// ends in a jump or `unreachable`.
    fn simple(&self, b: &Block) -> Option<String> {
        match &b.tail {
            tail @ (Tail::Jump { .. } | Tail::Unreachable | Tail::Trap) if b.lets.is_empty() => {
                Some(self.tail_text(tail))
            }
            _ => None,
        }
    }

    fn tail_text(&self, tail: &Tail) -> String {
        match tail {
            Tail::Jump {
                target,
                args,
                source,
            } => format!(
                "jump {}({}){}",
                self.label(*target),
                self.atoms(args),
                at(*source)
            ),
            Tail::Trap => "trap".to_string(),
            _ => "unreachable".to_string(),
        }
    }

    fn block(&mut self, b: &Block, depth: usize) {
        for l in &b.lets {
            let binds = l
                .binds
                .iter()
                .map(|&v| self.binder(v))
                .collect::<Vec<_>>()
                .join(", ");
            let space = if binds.is_empty() { "" } else { " " };
            let text = format!(
                "let {binds}{space}= {}({}){}",
                self.names.op(&l.op),
                self.atoms(&l.args),
                at(l.source)
            );
            self.line(depth, &text);
        }
        match &b.tail {
            Tail::Join { rec, defs, body } => {
                for (i, def) in defs.iter().enumerate() {
                    let keyword = match (i, rec) {
                        (0, true) => "join rec",
                        (0, false) => "join",
                        _ => "and",
                    };
                    let params = def
                        .params
                        .iter()
                        .map(|&v| self.binder(v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let head = format!("{keyword} {}({params}) =", self.label(def.label));
                    self.nested(depth, &head, &def.body);
                }
                match self.simple(body) {
                    Some(text) => self.line(depth, &format!("in {text}")),
                    None => {
                        self.line(depth, "in");
                        self.block(body, depth);
                    }
                }
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                self.line(depth, &format!("case {} {{", self.atom(scrut)));
                for arm in arms {
                    let tag = match arm.tag {
                        Tag::Int(n) => n.to_string(),
                        Tag::Ctor(c) => self.names.ctor(c),
                    };
                    self.nested(depth + 1, &format!("{tag} =>"), &arm.body);
                }
                if let Some(default) = default {
                    self.nested(depth + 1, "_ =>", default);
                }
                self.line(depth, "}");
            }
            tail @ (Tail::Jump { .. } | Tail::Unreachable | Tail::Trap) => {
                let text = self.tail_text(tail);
                self.line(depth, &text);
            }
        }
    }
}

fn at(source: Option<SourceId>) -> String {
    source.map_or(String::new(), |s| format!(" @{s}"))
}

pub(crate) fn atom(a: &Atom) -> String {
    match a {
        Atom::Var(v) => v.to_string(),
        Atom::Lit(lit) => lit_text(lit),
    }
}

/// A literal. Finite floats print in the shortest form that reads back
/// exactly; NaNs and infinities print as their bits.
pub(crate) fn lit_text(lit: &Lit) -> String {
    match *lit {
        Lit::I8(n) => format!("{n}i8"),
        Lit::I16(n) => format!("{n}i16"),
        Lit::I32(n) => format!("{n}i32"),
        Lit::I64(n) => format!("{n}i64"),
        Lit::F32(bits) => {
            let x = f32::from_bits(bits);
            if x.is_finite() {
                format!("{x:?}f32")
            } else {
                format!("f32_bits({bits})")
            }
        }
        Lit::F64(bits) => {
            let x = f64::from_bits(bits);
            if x.is_finite() {
                format!("{x:?}f64")
            } else {
                format!("f64_bits({bits})")
            }
        }
    }
}

fn string(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The bytes of an array that's only `i8` literals, if it has any: an
/// empty one is written `[]`, as for any element type.
fn bytes(elems: &[ConstValue]) -> Option<Vec<u8>> {
    if elems.is_empty() {
        return None;
    }
    elems
        .iter()
        .map(|e| match e {
            ConstValue::Lit(Lit::I8(n)) => Some(*n as u8),
            _ => None,
        })
        .collect()
}

/// `b"hi\n"`. What's valid UTF-8 is written as its characters, other than
/// quotes, backslashes and control characters, and every other byte as an
/// escape.
fn byte_string(bytes: &[u8]) -> String {
    let mut out = String::from("b\"");
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\0' => out.push_str("\\0"),
                c if c.is_control() => {
                    let mut buf = [0; 4];
                    for b in c.encode_utf8(&mut buf).bytes() {
                        let _ = write!(out, "\\x{b:02x}");
                    }
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out.push('"');
    out
}

fn sign(s: Sign) -> &'static str {
    match s {
        Sign::Signed => "s",
        Sign::Unsigned => "u",
    }
}

pub(super) fn prim_name(p: &Prim) -> String {
    let signed = |name: &str, s: Sign| format!("{name}.{}", sign(s));
    match p {
        Prim::Add => "add".into(),
        Prim::Sub => "sub".into(),
        Prim::Mul => "mul".into(),
        Prim::Div(s) => signed("div", *s),
        Prim::Rem(s) => signed("rem", *s),
        Prim::Neg => "neg".into(),
        Prim::And => "and".into(),
        Prim::Or => "or".into(),
        Prim::Xor => "xor".into(),
        Prim::Shl => "shl".into(),
        Prim::Shr(s) => signed("shr", *s),
        Prim::Eq => "eq".into(),
        Prim::Ne => "ne".into(),
        Prim::Lt(s) => signed("lt", *s),
        Prim::Le(s) => signed("le", *s),
        Prim::Gt(s) => signed("gt", *s),
        Prim::Ge(s) => signed("ge", *s),
        Prim::Abs => "abs".into(),
        Prim::Sqrt => "sqrt".into(),
        Prim::Min => "min".into(),
        Prim::Max => "max".into(),
        Prim::Convert(ty, s) => format!("convert.{}[{ty}]", sign(*s)),
        Prim::Reinterpret(ty) => format!("reinterpret[{ty}]"),
    }
}

/// Mutability arguments and rows, like `[imm, 'm1, effect20]`, or nothing
/// if there are none.
fn margs_text(margs: &[Mutability], rows: &[Row]) -> String {
    if margs.is_empty() && rows.is_empty() {
        return String::new();
    }
    let all: Vec<String> = margs
        .iter()
        .map(Mutability::to_string)
        .chain(rows.iter().map(Row::to_string))
        .collect();
    format!("[{}]", all.join(", "))
}

/// The parameters a declaration declares, like `['m1, 'm2, E1]`, or
/// nothing if it has none.
fn param_list(muts: u32, effects: u32) -> String {
    let all: Vec<String> = (1..=muts)
        .map(|i| Mutability::Param(i).to_string())
        .chain((1..=effects).map(|i| format!("E{i}")))
        .collect();
    if all.is_empty() {
        String::new()
    } else {
        format!("[{}]", all.join(", "))
    }
}

/// What comes before an op's arguments: its name, and the IDs or types
/// it takes.
pub(crate) fn op_name(op: &Op) -> String {
    match op {
        Op::Prim(p) => prim_name(p),
        Op::Call(f, margs, rows) => format!("call {f}{}", margs_text(margs, rows)),
        Op::CallExtern(e) => format!("call_extern {e}"),
        Op::Closure(f, margs, rows) => format!("closure {f}{}", margs_text(margs, rows)),
        Op::Apply => "apply".into(),
        Op::New(c) => format!("new {c}"),
        Op::Const(id) => format!("const {id}"),
        Op::Undefined => "undefined".into(),
        Op::Get(c, i) => format!("get {c}.{i}"),
        Op::Set(c, i) => format!("set {c}.{i}"),
        Op::ArrayNew => "array_new".into(),
        Op::ArrayFill => "array_fill".into(),
        Op::ArrayAlloc => "array_alloc".into(),
        Op::ArrayCopy => "array_copy".into(),
        Op::ArrayGet => "array_get".into(),
        Op::ArraySet => "array_set".into(),
        Op::ArrayLen => "array_len".into(),
        Op::ArrayClone => "array_clone".into(),
        Op::RefNew => "ref_new".into(),
        Op::RefGet => "ref_get".into(),
        Op::RefSet => "ref_set".into(),
        Op::Box(ty) => format!("box[{ty}]"),
        Op::Unbox(ty) => format!("unbox[{ty}]"),
        Op::ArrayFreeze => "array_freeze".into(),
        Op::Handle(h, rows) => format!("handle {h}{}", margs_text(&[], rows)),
        Op::Perform(e, i) => format!("perform {e}.{i}"),
        Op::Resume => "resume".into(),
        Op::Discontinue(e, i) => format!("discontinue {e}.{i}"),
        Op::Dup => "dup".into(),
        Op::Drop => "drop".into(),
        Op::IsUnique => "is_unique".into(),
        Op::Free => "free".into(),
        Op::DecRef => "decref".into(),
        Op::DropReuse => "drop_reuse".into(),
        Op::ReuseToken => "reuse_token".into(),
        Op::Reuse(c) => format!("reuse {c}"),
    }
}
