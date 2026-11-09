//! Lowers Conduit IR to LLVM IR, as text that `clang` or `llc` compile.
//!
//! An experiment, and a first step towards a native backend: it handles
//! the scalar part of the IR, functions, calls, join points, `case` and
//! the integer and float ops, and says what it doesn't handle yet, which
//! is objects, closures, handlers and constants. See `docs/llvm.md`.
//!
//! - **Functions** are `tailcc`, LLVM's calling convention that guarantees
//!   a call marked `tail` right before a `ret` is a tail call, as Conduit
//!   promises (see `conduit_ir::tail`). Several results are a struct.
//! - **Variables** each get an `alloca` in the entry block, which every
//!   binding stores to and every use loads from, so join point parameters
//!   need no phi nodes: LLVM's `mem2reg` makes SSA of them again.
//! - **Join points** are basic blocks, and a jump stores its arguments in
//!   the parameters' slots and branches. A `case` is a `switch`.
//! - **Ops** follow Conduit's semantics where LLVM's differ: integer
//!   division by 0 gives 0 and the remainder the dividend, signed
//!   division that overflows traps, shift amounts are taken modulo the
//!   width, and `min` and `max` give NaN if either operand is NaN.

use std::fmt::Write;

use conduit_ir::{
    Atom, Block, ExternId, Func, FuncId, JoinDef, Label, Link, Lit, Module, Op, Prim, Sign, Tag,
    Tail, Ty, Var,
};

/// Why a module can't be lowered: an item, and what's wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub func: Option<FuncId>,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.func {
            Some(id) => write!(f, "{id}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

/// The LLVM symbol of function `f`.
pub fn symbol(f: FuncId) -> String {
    format!("@fn{}", f.0)
}

/// The LLVM type of a value of type `ty`, if it's one this backend
/// handles yet.
pub fn ty(ty: &Ty) -> Result<&'static str, String> {
    Ok(match ty {
        Ty::I8 => "i8",
        Ty::I16 => "i16",
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::F32 => "float",
        Ty::F64 => "double",
        other => return Err(format!("values of type {other} aren't supported yet")),
    })
}

/// What a function giving `results` returns: `void`, the one result, or a
/// struct of them.
pub fn ret_ty(results: &[Ty]) -> Result<String, String> {
    Ok(match results {
        [] => "void".into(),
        [one] => ty(one)?.into(),
        many => {
            let tys: Vec<&str> = many.iter().map(ty).collect::<Result<_, _>>()?;
            format!("{{ {} }}", tys.join(", "))
        }
    })
}

/// A literal as LLVM writes it. Floats are hexadecimal, of the double
/// that has the same value, which is how LLVM writes a `float` too.
pub fn lit(l: &Lit) -> String {
    match *l {
        Lit::I8(n) => n.to_string(),
        Lit::I16(n) => n.to_string(),
        Lit::I32(n) => n.to_string(),
        Lit::I64(n) => n.to_string(),
        Lit::F32(bits) => format!("0x{:016X}", f64::from(f32::from_bits(bits)).to_bits()),
        Lit::F64(bits) => format!("0x{bits:016X}"),
    }
}

fn lit_ty(l: &Lit) -> Ty {
    match l {
        Lit::I8(_) => Ty::I8,
        Lit::I16(_) => Ty::I16,
        Lit::I32(_) => Ty::I32,
        Lit::I64(_) => Ty::I64,
        Lit::F32(_) => Ty::F32,
        Lit::F64(_) => Ty::F64,
    }
}

/// The module as LLVM IR text, or what it has that isn't supported yet.
/// Each function `f` is `@fn<f>`, `tailcc`, and each `#[export(c, symbol
/// = "s")]` gets a C-callable `@s` that calls it. Externs must be
/// `#[link(c, symbol = "..")]`.
pub fn emit_module(m: &Module) -> Result<String, Error> {
    let err = |func: Option<FuncId>, message: String| Error { func, message };
    if !m.handlers.is_empty() {
        return Err(err(
            None,
            "handlers aren't supported yet: lower effects first".into(),
        ));
    }
    let mut out = String::new();
    writeln!(out, "; Lowered from Conduit IR by conduit-llvm.").unwrap();
    writeln!(out, "declare void @llvm.trap() noreturn nounwind").unwrap();
    for t in ["float", "double"] {
        let s = if t == "float" { "f32" } else { "f64" };
        for op in ["fabs", "sqrt"] {
            writeln!(out, "declare {t} @llvm.{op}.{s}({t})").unwrap();
        }
        for op in ["minimum", "maximum"] {
            writeln!(out, "declare {t} @llvm.{op}.{s}({t}, {t})").unwrap();
        }
    }
    for (&id, ext) in &m.externs {
        let Link::C { symbol, .. } = &ext.link else {
            continue;
        };
        let e = |message| err(None, format!("extern {id}: {message}"));
        let params: Vec<&str> = ext
            .sig
            .params
            .iter()
            .map(ty)
            .collect::<Result<_, _>>()
            .map_err(e)?;
        let ret = ret_ty(&ext.sig.results).map_err(e)?;
        writeln!(out, "declare {ret} @{symbol}({})", params.join(", ")).unwrap();
    }
    for (&id, f) in &m.funcs {
        let text = Lower::new(m, id, f)
            .and_then(|mut l| l.func())
            .map_err(|message| err(Some(id), message))?;
        out.push_str(&text);
    }
    for (&id, export) in &m.exports {
        let conduit_ir::Export::C { symbol } = export else {
            continue;
        };
        let f = &m.funcs[&id];
        let e = |message| err(Some(id), message);
        let ret = ret_ty(&f.results).map_err(e)?;
        let params: Vec<String> = (f.params.iter().enumerate())
            .map(|(i, v)| ty(f.var_ty(*v)).map(|t| format!("{t} %p{i}")))
            .collect::<Result<_, _>>()
            .map_err(e)?;
        let args: Vec<String> = (f.params.iter().enumerate())
            .map(|(i, v)| format!("{} %p{i}", ty(f.var_ty(*v)).unwrap()))
            .collect();
        writeln!(out, "\ndefine {ret} @{symbol}({}) {{", params.join(", ")).unwrap();
        if ret == "void" {
            writeln!(
                out,
                "  call tailcc void {}({})",
                self::symbol(id),
                args.join(", ")
            )
            .unwrap();
            writeln!(out, "  ret void\n}}").unwrap();
        } else {
            writeln!(
                out,
                "  %r = call tailcc {ret} {}({})",
                self::symbol(id),
                args.join(", ")
            )
            .unwrap();
            writeln!(out, "  ret {ret} %r\n}}").unwrap();
        }
    }
    Ok(out)
}

/// The lowering of one function.
struct Lower<'m> {
    m: &'m Module,
    id: FuncId,
    f: &'m Func,
    defs: Vec<Option<&'m JoinDef>>,
    ret: String,
    out: String,
    /// The next temporary and the next block.
    temps: u32,
    blocks: u32,
}

impl<'m> Lower<'m> {
    fn new(m: &'m Module, id: FuncId, f: &'m Func) -> Result<Lower<'m>, String> {
        Ok(Lower {
            m,
            id,
            f,
            defs: conduit_ir::tail::join_defs(&f.body, f.labels),
            ret: ret_ty(&f.results)?,
            out: String::new(),
            temps: 0,
            blocks: 0,
        })
    }

    fn func(&mut self) -> Result<String, String> {
        let params: Vec<String> = (self.f.params.iter())
            .map(|v| Ok(format!("{} %a{}", ty(self.f.var_ty(*v))?, v.0)))
            .collect::<Result<_, String>>()?;
        let mut head = String::new();
        writeln!(
            head,
            "\ndefine tailcc {} {}({}) {{\nentry:",
            self.ret,
            symbol(self.id),
            params.join(", ")
        )
        .unwrap();
        // A slot for each variable, and the parameters stored in theirs.
        for (i, t) in self.f.vars.iter().enumerate() {
            let t = ty(t).map_err(|e| format!("%{i}: {e}"))?;
            writeln!(head, "  %v{i} = alloca {t}").unwrap();
        }
        for v in &self.f.params {
            let t = ty(self.f.var_ty(*v))?;
            writeln!(head, "  store {t} %a{0}, ptr %v{0}", v.0).unwrap();
        }
        self.block(&self.f.body)?;
        Ok(format!("{head}{}}}\n", self.out))
    }

    fn line(&mut self, s: impl AsRef<str>) {
        self.out.push_str("  ");
        self.out.push_str(s.as_ref());
        self.out.push('\n');
    }

    fn temp(&mut self) -> String {
        self.temps += 1;
        format!("%t{}", self.temps)
    }

    fn new_block(&mut self) -> String {
        self.blocks += 1;
        format!("b{}", self.blocks)
    }

    fn start(&mut self, name: &str) {
        writeln!(self.out, "{name}:").unwrap();
    }

    fn atom_ty(&self, a: &Atom) -> Ty {
        match a {
            Atom::Var(v) => self.f.var_ty(*v).clone(),
            Atom::Lit(l) => lit_ty(l),
        }
    }

    /// An atom's value, loaded if it's a variable.
    fn value(&mut self, a: &Atom) -> Result<String, String> {
        match a {
            Atom::Lit(l) => Ok(lit(l)),
            Atom::Var(v) => {
                let t = ty(self.f.var_ty(*v))?;
                let x = self.temp();
                self.line(format!("{x} = load {t}, ptr %v{}", v.0));
                Ok(x)
            }
        }
    }

    fn store(&mut self, v: Var, value: &str) -> Result<(), String> {
        let t = ty(self.f.var_ty(v))?;
        self.line(format!("store {t} {value}, ptr %v{}", v.0));
        Ok(())
    }

    fn block(&mut self, b: &Block) -> Result<(), String> {
        conduit_ir::deep(|| {
            for (i, l) in b.lets.iter().enumerate() {
                let last = i + 1 == b.lets.len();
                if last
                    && let Op::Call(g, ..) = l.op
                    && conduit_ir::tail::returns(&l.binds, &b.tail, &self.defs)
                    && self
                        .m
                        .funcs
                        .get(&g)
                        .is_some_and(|g| g.results == self.f.results)
                {
                    return self.tail_call(g, &l.args);
                }
                self.let_(&l.op, &l.args, &l.binds)?;
            }
            self.tail(&b.tail)
        })
    }

    /// A call that returns what it gives, which `tailcc` makes a tail call.
    fn tail_call(&mut self, g: FuncId, args: &[Atom]) -> Result<(), String> {
        let args = self.args(args)?;
        let ret = self.ret.clone();
        if ret == "void" {
            self.line(format!("tail call tailcc void {}({args})", symbol(g)));
            self.line("ret void");
        } else {
            let r = self.temp();
            self.line(format!(
                "{r} = tail call tailcc {ret} {}({args})",
                symbol(g)
            ));
            self.line(format!("ret {ret} {r}"));
        }
        Ok(())
    }

    fn args(&mut self, args: &[Atom]) -> Result<String, String> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            let t = ty(&self.atom_ty(a))?;
            let v = self.value(a)?;
            out.push(format!("{t} {v}"));
        }
        Ok(out.join(", "))
    }

    /// The results of a call, of type `ret` from `results`, into `binds`.
    fn bind_results(&mut self, call: String, results: &[Ty], binds: &[Var]) -> Result<(), String> {
        match results {
            [] => self.line(call),
            [_] => {
                let r = self.temp();
                self.line(format!("{r} = {call}"));
                if let Some(&b) = binds.first() {
                    self.store(b, &r)?;
                }
            }
            many => {
                let r = self.temp();
                self.line(format!("{r} = {call}"));
                let agg = ret_ty(many)?;
                for (i, &b) in binds.iter().enumerate() {
                    let x = self.temp();
                    self.line(format!("{x} = extractvalue {agg} {r}, {i}"));
                    self.store(b, &x)?;
                }
            }
        }
        Ok(())
    }

    fn let_(&mut self, op: &Op, args: &[Atom], binds: &[Var]) -> Result<(), String> {
        match op {
            Op::Prim(p) => {
                let r = self.prim(p, args)?;
                if let Some(&b) = binds.first() {
                    self.store(b, &r)?;
                }
            }
            Op::Call(g, ..) => {
                let callee = self
                    .m
                    .funcs
                    .get(g)
                    .ok_or_else(|| format!("unknown function {g}"))?;
                let ret = ret_ty(&callee.results)?;
                let args = self.args(args)?;
                let call = format!("call tailcc {ret} {}({args})", symbol(*g));
                let results = callee.results.clone();
                self.bind_results(call, &results, binds)?;
            }
            Op::CallExtern(e) => self.call_extern(*e, args, binds)?,
            // Nothing to store: the slot is never read before a store.
            Op::Undefined => {}
            other => return Err(format!("{other:?} isn't supported yet")),
        }
        Ok(())
    }

    fn call_extern(&mut self, e: ExternId, args: &[Atom], binds: &[Var]) -> Result<(), String> {
        let ext = self
            .m
            .externs
            .get(&e)
            .ok_or_else(|| format!("unknown extern {e}"))?;
        let Link::C { symbol, .. } = &ext.link else {
            return Err(format!(
                "extern {e} has a {} link: only `#[link(c, ..)]` ones can be called",
                ext.link.kind()
            ));
        };
        let ret = ret_ty(&ext.sig.results)?;
        let args = self.args(args)?;
        let call = format!("call {ret} @{symbol}({args})");
        let results = ext.sig.results.clone();
        self.bind_results(call, &results, binds)
    }

    fn tail(&mut self, t: &Tail) -> Result<(), String> {
        match t {
            Tail::Join { defs, body, .. } => {
                self.block(body)?;
                for d in defs {
                    self.start(&format!("j{}", d.label.0));
                    self.block(&d.body)?;
                }
            }
            Tail::Jump { target, args, .. } if *target == Label::RET => self.ret(args)?,
            Tail::Jump { target, args, .. } => {
                let def = (self.defs.get(target.0 as usize).copied().flatten())
                    .ok_or_else(|| format!("{target} isn't defined"))?;
                // Every argument first, since they may be the parameters.
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.value(a)?);
                }
                for (p, v) in def.params.clone().iter().zip(values) {
                    self.store(*p, &v)?;
                }
                self.line(format!("br label %j{}", target.0));
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                let t = ty(&self.atom_ty(scrut))?;
                let s = self.value(scrut)?;
                let names: Vec<String> = arms.iter().map(|_| self.new_block()).collect();
                let other = self.new_block();
                let mut cases = Vec::with_capacity(arms.len());
                for (arm, name) in arms.iter().zip(&names) {
                    let Tag::Int(n) = arm.tag else {
                        return Err("a case on data isn't supported yet".into());
                    };
                    cases.push(format!("{t} {n}, label %{name}"));
                }
                self.line(format!(
                    "switch {t} {s}, label %{other} [ {} ]",
                    cases.join(" ")
                ));
                for (arm, name) in arms.iter().zip(&names) {
                    self.start(name);
                    self.block(&arm.body)?;
                }
                self.start(&other);
                match default {
                    Some(d) => self.block(d)?,
                    None => self.line("unreachable"),
                }
            }
            Tail::Unreachable => self.line("unreachable"),
            Tail::Trap => {
                self.line("call void @llvm.trap()");
                self.line("unreachable");
            }
        }
        Ok(())
    }

    fn ret(&mut self, args: &[Atom]) -> Result<(), String> {
        let ret = self.ret.clone();
        match args {
            [] => self.line("ret void"),
            [a] => {
                let v = self.value(a)?;
                self.line(format!("ret {ret} {v}"));
            }
            many => {
                let mut agg = "undef".to_string();
                for (i, a) in many.iter().enumerate() {
                    let t = ty(&self.atom_ty(a))?;
                    let v = self.value(a)?;
                    let next = self.temp();
                    self.line(format!("{next} = insertvalue {ret} {agg}, {t} {v}, {i}"));
                    agg = next;
                }
                self.line(format!("ret {ret} {agg}"));
            }
        }
        Ok(())
    }

    /// Branches to a trap if `cond` holds, and goes on in a new block.
    fn trap_if(&mut self, cond: &str) {
        let (trap, ok) = (self.new_block(), self.new_block());
        self.line(format!("br i1 {cond}, label %{trap}, label %{ok}"));
        self.start(&trap);
        self.line("call void @llvm.trap()");
        self.line("unreachable");
        self.start(&ok);
    }

    fn prim(&mut self, p: &Prim, args: &[Atom]) -> Result<String, String> {
        let at = self.atom_ty(&args[0]);
        let t = ty(&at)?;
        let float = matches!(at, Ty::F32 | Ty::F64);
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.value(a)?);
        }
        let bin = |me: &mut Self, op: &str| {
            let r = me.temp();
            me.line(format!("{r} = {op} {t} {}, {}", vals[0], vals[1]));
            r
        };
        let suffix = if at == Ty::F32 { "f32" } else { "f64" };
        Ok(match p {
            Prim::Add => bin(self, if float { "fadd" } else { "add" }),
            Prim::Sub => bin(self, if float { "fsub" } else { "sub" }),
            Prim::Mul => bin(self, if float { "fmul" } else { "mul" }),
            Prim::Div(_) if float => bin(self, "fdiv"),
            Prim::Div(sign) | Prim::Rem(sign) => {
                let div = matches!(p, Prim::Div(_));
                let (x, y) = (vals[0].clone(), vals[1].clone());
                let z = self.temp();
                self.line(format!("{z} = icmp eq {t} {y}, 0"));
                // A signed remainder by -1 is 0, which LLVM leaves undefined
                // for the smallest value; a signed division by -1 of it traps.
                let minus_one = self.temp();
                self.line(format!("{minus_one} = icmp eq {t} {y}, -1"));
                if div && *sign == Sign::Signed {
                    let bits = int_bits(&at);
                    let min = self.temp();
                    self.line(format!(
                        "{min} = icmp eq {t} {x}, {}",
                        i64::MIN >> (64 - bits)
                    ));
                    let ov = self.temp();
                    self.line(format!("{ov} = and i1 {min}, {minus_one}"));
                    self.trap_if(&ov);
                }
                let safe_cond = if !div && *sign == Sign::Signed {
                    let c = self.temp();
                    self.line(format!("{c} = or i1 {z}, {minus_one}"));
                    c
                } else {
                    z.clone()
                };
                let d = self.temp();
                self.line(format!("{d} = select i1 {safe_cond}, {t} 1, {t} {y}"));
                let op = match (div, sign) {
                    (true, Sign::Signed) => "sdiv",
                    (true, Sign::Unsigned) => "udiv",
                    (false, Sign::Signed) => "srem",
                    (false, Sign::Unsigned) => "urem",
                };
                let q = self.temp();
                self.line(format!("{q} = {op} {t} {x}, {d}"));
                let r = self.temp();
                if div {
                    self.line(format!("{r} = select i1 {z}, {t} 0, {t} {q}"));
                } else {
                    let q2 = if *sign == Sign::Signed {
                        let q2 = self.temp();
                        self.line(format!("{q2} = select i1 {minus_one}, {t} 0, {t} {q}"));
                        q2
                    } else {
                        q
                    };
                    self.line(format!("{r} = select i1 {z}, {t} {x}, {t} {q2}"));
                }
                r
            }
            Prim::Neg if float => {
                let r = self.temp();
                self.line(format!("{r} = fneg {t} {}", vals[0]));
                r
            }
            Prim::Neg => {
                let r = self.temp();
                self.line(format!("{r} = sub {t} 0, {}", vals[0]));
                r
            }
            Prim::And => bin(self, "and"),
            Prim::Or => bin(self, "or"),
            Prim::Xor => bin(self, "xor"),
            Prim::Shl | Prim::Shr(_) => {
                let n = self.temp();
                self.line(format!("{n} = and {t} {}, {}", vals[1], int_bits(&at) - 1));
                let op = match p {
                    Prim::Shl => "shl",
                    Prim::Shr(Sign::Signed) => "ashr",
                    _ => "lshr",
                };
                let r = self.temp();
                self.line(format!("{r} = {op} {t} {}, {n}", vals[0]));
                r
            }
            Prim::Eq | Prim::Ne | Prim::Lt(_) | Prim::Le(_) | Prim::Gt(_) | Prim::Ge(_) => {
                let cmp = if float {
                    let c = match p {
                        Prim::Eq => "oeq",
                        Prim::Ne => "une",
                        Prim::Lt(_) => "olt",
                        Prim::Le(_) => "ole",
                        Prim::Gt(_) => "ogt",
                        _ => "oge",
                    };
                    format!("fcmp {c}")
                } else {
                    let s = |sign: &Sign| if *sign == Sign::Signed { 's' } else { 'u' };
                    let c = match p {
                        Prim::Eq => "eq".to_string(),
                        Prim::Ne => "ne".to_string(),
                        Prim::Lt(sg) => format!("{}lt", s(sg)),
                        Prim::Le(sg) => format!("{}le", s(sg)),
                        Prim::Gt(sg) => format!("{}gt", s(sg)),
                        Prim::Ge(sg) => format!("{}ge", s(sg)),
                        _ => unreachable!(),
                    };
                    format!("icmp {c}")
                };
                let b = self.temp();
                self.line(format!("{b} = {cmp} {t} {}, {}", vals[0], vals[1]));
                let r = self.temp();
                self.line(format!("{r} = zext i1 {b} to i32"));
                r
            }
            Prim::Abs | Prim::Sqrt => {
                let f = if *p == Prim::Abs { "fabs" } else { "sqrt" };
                let r = self.temp();
                self.line(format!(
                    "{r} = call {t} @llvm.{f}.{suffix}({t} {})",
                    vals[0]
                ));
                r
            }
            Prim::Min | Prim::Max => {
                let f = if *p == Prim::Min {
                    "minimum"
                } else {
                    "maximum"
                };
                let r = self.temp();
                self.line(format!(
                    "{r} = call {t} @llvm.{f}.{suffix}({t} {}, {t} {})",
                    vals[0], vals[1]
                ));
                r
            }
            Prim::Convert(to, sign) => self.convert(&at, to, *sign, &vals[0])?,
            Prim::Reinterpret(to) => {
                let r = self.temp();
                self.line(format!("{r} = bitcast {t} {} to {}", vals[0], ty(to)?));
                r
            }
        })
    }

    fn convert(&mut self, from: &Ty, to: &Ty, sign: Sign, x: &str) -> Result<String, String> {
        let (f, t) = (ty(from)?, ty(to)?);
        let float = |t: &Ty| matches!(t, Ty::F32 | Ty::F64);
        let op = match (float(from), float(to)) {
            (false, false) => match int_bits(from).cmp(&int_bits(to)) {
                std::cmp::Ordering::Equal => return Ok(x.to_string()),
                std::cmp::Ordering::Greater => "trunc",
                std::cmp::Ordering::Less if sign == Sign::Signed => "sext",
                std::cmp::Ordering::Less => "zext",
            },
            (false, true) if sign == Sign::Signed => "sitofp",
            (false, true) => "uitofp",
            (true, true) if from == to => return Ok(x.to_string()),
            (true, true) if *from == Ty::F32 => "fpext",
            (true, true) => "fptrunc",
            (true, false) => {
                return Err("converting a float to an integer isn't supported yet".into());
            }
        };
        let r = self.temp();
        self.line(format!("{r} = {op} {f} {x} to {t}"));
        Ok(r)
    }
}

fn int_bits(t: &Ty) -> u32 {
    match t {
        Ty::I8 => 8,
        Ty::I16 => 16,
        Ty::I32 => 32,
        _ => 64,
    }
}
