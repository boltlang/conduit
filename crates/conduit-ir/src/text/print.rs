use std::collections::BTreeSet;
use std::fmt::Write;

use crate::*;

pub(super) fn module(m: &Module) -> String {
    let mut items = Vec::new();
    for (id, decl) in &m.data {
        let params: Vec<String> = (1..=decl.params)
            .map(|i| Mutability::Param(i).to_string())
            .collect();
        let mut s = if params.is_empty() {
            format!("data {id} {{\n")
        } else {
            format!("data {id}[{}] {{\n", params.join(", "))
        };
        for ctor in &decl.ctors {
            let _ = writeln!(s, "  ({})", types(&ctor.fields));
        }
        s.push('}');
        items.push(s);
    }
    for (id, ext) in &m.externs {
        items.push(format!(
            "extern {id} {} {} {}",
            string(&ext.module),
            string(&ext.name),
            ext.sig
        ));
    }
    for (id, effect) in &m.effects {
        let mut s = format!("effect {id} {{\n");
        for sig in &effect.ops {
            let _ = writeln!(s, "  {sig}");
        }
        s.push('}');
        items.push(s);
    }
    for (id, h) in &m.handlers {
        let ops = h
            .ops
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        items.push(format!(
            "handler {id} for {} env({}) ops({ops}) ret {}",
            h.effect,
            types(&h.env),
            h.ret
        ));
    }
    for (&id, f) in &m.funcs {
        items.push(func(id, f));
    }
    let mut out = items.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

pub(super) fn func(id: FuncId, f: &Func) -> String {
    let mut p = Printer {
        f,
        out: String::new(),
    };
    let params = f
        .params
        .iter()
        .map(|&v| p.binder(v))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = write!(p.out, "fn {id}({params}) -> ({})", types(&f.results));

    let mut bound = BTreeSet::new();
    let mut max_label = 0;
    walk(&f.body, &mut |b| {
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
    p.out
}

/// Calls `visit` on every block, without recursion.
fn walk<'a>(b: &'a Block, visit: &mut impl FnMut(&'a Block)) {
    let mut todo = vec![b];
    while let Some(b) = todo.pop() {
        visit(b);
        match &b.tail {
            Tail::Join { defs, body, .. } => {
                todo.extend(defs.iter().map(|d| &d.body));
                todo.push(body);
            }
            Tail::Case { arms, default, .. } => {
                todo.extend(arms.iter().map(|a| &a.body));
                todo.extend(default.as_deref());
            }
            Tail::Jump { .. } | Tail::Unreachable => {}
        }
    }
}

struct Printer<'a> {
    f: &'a Func,
    out: String,
}

impl Printer<'_> {
    /// `%3: i32`. A variable without a type, which only broken IR has,
    /// prints as `%3: ?`, which doesn't parse.
    fn binder(&self, v: Var) -> String {
        match self.f.vars.get(v.0 as usize) {
            Some(ty) => format!("{v}: {ty}"),
            None => format!("{v}: ?"),
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
        match simple(b) {
            Some(text) => self.line(depth, &format!("{head} {text}")),
            None => {
                self.line(depth, head);
                self.block(b, depth + 1);
            }
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
                op_name(&l.op),
                atoms(&l.args),
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
                    self.nested(
                        depth,
                        &format!("{keyword} {}({params}) =", def.label),
                        &def.body,
                    );
                }
                match simple(body) {
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
                self.line(depth, &format!("case {} {{", atom(scrut)));
                for arm in arms {
                    let tag = match arm.tag {
                        Tag::Int(n) => n.to_string(),
                        Tag::Ctor(c) => c.to_string(),
                    };
                    self.nested(depth + 1, &format!("{tag} =>"), &arm.body);
                }
                if let Some(default) = default {
                    self.nested(depth + 1, "_ =>", default);
                }
                self.line(depth, "}");
            }
            tail @ (Tail::Jump { .. } | Tail::Unreachable) => {
                let text = tail_text(tail);
                self.line(depth, &text);
            }
        }
    }
}

/// The text of a block that fits on one line: one without `let`s that
/// ends in a jump or `unreachable`.
fn simple(b: &Block) -> Option<String> {
    match &b.tail {
        tail @ (Tail::Jump { .. } | Tail::Unreachable) if b.lets.is_empty() => {
            Some(tail_text(tail))
        }
        _ => None,
    }
}

fn tail_text(tail: &Tail) -> String {
    match tail {
        Tail::Jump {
            target,
            args,
            source,
        } => format!("jump {target}({}){}", atoms(args), at(*source)),
        _ => "unreachable".to_string(),
    }
}

fn at(source: Option<SourceId>) -> String {
    source.map_or(String::new(), |s| format!(" @{s}"))
}

fn types(tys: &[Ty]) -> String {
    tys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn atoms(args: &[Atom]) -> String {
    args.iter().map(atom).collect::<Vec<_>>().join(", ")
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

/// What comes before an op's arguments: its name, and the IDs or types
/// it takes.
fn op_name(op: &Op) -> String {
    match op {
        Op::Prim(p) => prim_name(p),
        Op::Call(f) => format!("call {f}"),
        Op::CallExtern(e) => format!("call_extern {e}"),
        Op::Closure(f) => format!("closure {f}"),
        Op::Apply => "apply".into(),
        Op::New(c) => format!("new {c}"),
        Op::Get(c, i) => format!("get {c}.{i}"),
        Op::Set(c, i) => format!("set {c}.{i}"),
        Op::ArrayNew => "array_new".into(),
        Op::ArrayFill => "array_fill".into(),
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
        Op::Handle(h) => format!("handle {h}"),
        Op::Perform(e, i) => format!("perform {e}.{i}"),
        Op::Resume => "resume".into(),
    }
}
