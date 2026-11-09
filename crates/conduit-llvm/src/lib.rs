//! Lowers Conduit IR to LLVM IR, as text that `clang` or `llc` compile.
//!
//! An experiment, and a first step towards a native backend: it handles
//! functions, calls, join points, `case`, the integer and float ops,
//! objects (data, arrays, refs, `any`, closures) and constants, and says
//! what it doesn't handle yet: handlers, which lowering effects removes
//! first, host values, and freeing memory. See `docs/llvm.md`.
//!
//! - **Functions** are `tailcc`, LLVM's calling convention that guarantees
//!   a call marked `tail` right before a `ret` is a tail call, as Conduit
//!   promises (see `conduit_ir::tail`). Several results are a struct.
//! - **Variables** each get an `alloca` in the entry block, which every
//!   binding stores to and every use loads from, so join point parameters
//!   need no phi nodes: LLVM's `mem2reg` makes SSA of them again.
//! - **Join points** are basic blocks, and a jump stores its arguments in
//!   the parameters' slots and branches. A `case` is a `switch`.
//! - **Objects** are `ptr`s to memory from `malloc`, laid out as in the
//!   Wasm backend: a header word for the count, then a word with the
//!   constructor, the length or the kind, then the fields from offset 8,
//!   each at a multiple of its size. A closure holds its entry function's
//!   address at 8, the function that frees it at 16, then its captures; an
//!   `any` holds its value at 8 and the drop of the value at 16. With
//!   [`Options::counts`], objects are counted and freed (see `counts`);
//!   without, nothing is freed, and the ops that count do nothing, as in
//!   the Wasm backend.
//! - **Ops** follow Conduit's semantics where LLVM's differ: integer
//!   division by 0 gives 0 and the remainder the dividend, signed
//!   division that overflows traps, shift amounts are taken modulo the
//!   width, `min` and `max` give NaN if either operand is NaN, and array
//!   accesses out of bounds trap.

mod counts;

use std::collections::BTreeSet;
use std::fmt::Write;

use counts::Kind;

use conduit_ir::{
    Atom, Block, ConstId, ConstValue, CtorId, ExternId, Func, FuncId, JoinDef, Label, Link, Lit,
    Module, Op, Prim, Sign, Tag, Tail, Ty, Var,
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

/// How [`emit_module_with`] lowers a module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Whether the module's counts are explicit, as
    /// `conduit_opt::ownership::insert_counts` makes them, so that memory
    /// is freed: `dup`, `drop` and the ops that count for themselves count.
    /// The module then has `@conduit_live`, the number of objects allocated
    /// and not freed. Otherwise those ops do nothing, and nothing is freed.
    pub counts: bool,
}

/// The LLVM symbol of function `f`.
pub fn symbol(f: FuncId) -> String {
    format!("@fn{}", f.0)
}

/// The entry function of `f`'s closures with `n` captures.
fn entry_symbol(f: FuncId, n: usize) -> String {
    format!("@fn{}_entry{n}", f.0)
}

/// The function that frees a closure of `f` with `n` captures.
fn free_symbol(f: FuncId, n: usize) -> String {
    format!("@fn{}_free{n}", f.0)
}

fn const_symbol(c: ConstId) -> String {
    format!("@const{}", c.0)
}

/// The LLVM type of a value of type `ty`, if it's one this backend
/// handles yet: an object is a `ptr`.
pub fn ty(ty: &Ty) -> Result<&'static str, String> {
    Ok(match ty {
        Ty::I8 => "i8",
        Ty::I16 => "i16",
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::F32 => "float",
        Ty::F64 => "double",
        Ty::Data(..) | Ty::Array(..) | Ty::Fn(..) | Ty::Ref(_) | Ty::Any(_) | Ty::Reuse(_) => "ptr",
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

/// How many bytes a value of type `t` takes in an object: a scalar's size,
/// or 8 for an object's address.
fn size(t: &Ty) -> u64 {
    match t {
        Ty::I8 => 1,
        Ty::I16 => 2,
        Ty::I32 | Ty::F32 => 4,
        _ => 8,
    }
}

/// The offsets of values of these types, laid out from 8 each at a
/// multiple of its size, and the size of the whole object.
fn layout(tys: &[Ty]) -> (Vec<u64>, u64) {
    let mut at: u64 = 8;
    let offsets = tys
        .iter()
        .map(|t| {
            at = at.next_multiple_of(size(t));
            let o = at;
            at += size(t);
            o
        })
        .collect();
    (offsets, at.next_multiple_of(8))
}

/// The layout of a closure with these captures: its entry function's
/// address at 8, the address of the function that frees it at 16, then
/// the captures.
fn closure_layout(captures: &[Ty]) -> (Vec<u64>, u64) {
    let address = Ty::Any(conduit_ir::Mutability::Imm);
    let mut tys = vec![address.clone(), address];
    tys.extend(captures.iter().cloned());
    let (offsets, size) = layout(&tys);
    (offsets[2..].to_vec(), size)
}

/// The module as LLVM IR text, or what it has that isn't supported yet,
/// with the default [`Options`]: nothing is freed.
pub fn emit_module(m: &Module) -> Result<String, Error> {
    emit_module_with(m, &Options::default())
}

/// The module as LLVM IR text, or what it has that isn't supported yet.
/// Each function `f` is `@fn<f>`, `tailcc`, and each `#[export(c, symbol
/// = "s")]` gets a C-callable `@s` that calls it. Externs must be
/// `#[link(c, symbol = "..")]`.
pub fn emit_module_with(m: &Module, options: &Options) -> Result<String, Error> {
    let err = |func: Option<FuncId>, message: String| Error { func, message };
    if !m.handlers.is_empty() {
        return Err(err(
            None,
            "handlers aren't supported yet: lower effects first".into(),
        ));
    }
    let mut out = String::new();
    out.push_str("; Lowered from Conduit IR by conduit-llvm.\n");
    out.push_str("declare void @llvm.trap() noreturn nounwind\n");
    out.push_str("declare ptr @calloc(i64, i64)\n");
    out.push_str("declare void @free(ptr)\n");
    out.push_str("declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1)\n");
    for (t, s) in [("float", "f32"), ("double", "f64")] {
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
    let mut consts = String::new();
    let mut entries = BTreeSet::new();
    for (&c, decl) in &m.consts {
        if !matches!(decl.value, ConstValue::Lit(_)) {
            let name = const_symbol(c);
            const_global(m, &mut consts, &mut entries, &name, &decl.value, &decl.ty)
                .map_err(|message| err(None, format!("constant {c}: {message}")))?;
        }
    }
    out.push_str(&consts);
    let mut kinds = BTreeSet::new();
    for (&id, f) in &m.funcs {
        let mut l =
            Lower::new(m, id, f, options.counts).map_err(|message| err(Some(id), message))?;
        let text = l.func().map_err(|message| err(Some(id), message))?;
        out.push_str(&text);
        entries.extend(l.entries);
        kinds.extend(l.kinds);
    }
    for &(f, n) in &entries {
        out.push_str(&entry(m, f, n, options.counts).map_err(|message| err(Some(f), message))?);
        if options.counts {
            let caps = &m.funcs[&f].sig().params[..n];
            let (offsets, _) = closure_layout(caps);
            kinds.extend(caps.iter().filter_map(Kind::of));
            let text = counts::closure_free(&free_symbol(f, n), caps, &offsets)
                .map_err(|message| err(Some(f), message))?;
            out.push_str(&text);
        }
    }
    if options.counts {
        // Entry functions drop their closure.
        if !entries.is_empty() {
            kinds.insert(Kind::Closure);
        }
        out.push_str(counts::runtime());
        out.push_str(&counts::drops(m, &kinds).map_err(|message| err(None, message))?);
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
        let call = format!(
            "call tailcc {ret} {}({})",
            self::symbol(id),
            params.join(", ")
        );
        writeln!(out, "\ndefine {ret} @{symbol}({}) {{", params.join(", ")).unwrap();
        if ret == "void" {
            writeln!(out, "  {call}\n  ret void\n}}").unwrap();
        } else {
            writeln!(out, "  %r = {call}\n  ret {ret} %r\n}}").unwrap();
        }
    }
    Ok(out)
}

/// A global for constant `value` of type `t`, as an object laid out as
/// at run time, immortal: its header has the sign bit set, which counting
/// will skip. An object inside it is a global of its own. A closure's
/// entry function goes in `entries`.
fn const_global(
    m: &Module,
    out: &mut String,
    entries: &mut BTreeSet<(FuncId, usize)>,
    name: &str,
    value: &ConstValue,
    t: &Ty,
) -> Result<(), String> {
    // A closure's and an `any`'s pointers at 16 are never called, as
    // nothing frees an immortal object: they're null.
    let (word, tys, values, after): (u64, Vec<Ty>, &[ConstValue], Vec<String>) = match (value, t) {
        (ConstValue::New(c, fields), _) => {
            (c.index as u64, ctor_fields(m, *c)?, fields, Vec::new())
        }
        (ConstValue::Array(elems), Ty::Array(e, _)) => (
            elems.len() as u64,
            vec![(**e).clone(); elems.len()],
            elems,
            Vec::new(),
        ),
        (ConstValue::Closure(f), _) => {
            entries.insert((*f, 0));
            let entry = format!("ptr {}", entry_symbol(*f, 0));
            (0, Vec::new(), &[], vec![entry, "ptr null".into()])
        }
        (ConstValue::Box(bt, inner), _) => (
            0,
            vec![bt.clone()],
            std::slice::from_ref(&**inner),
            vec!["ptr null".into()],
        ),
        _ => return Err(format!("a constant of type {t} isn't supported yet")),
    };
    let (offsets, total) = layout(&tys);
    let mut parts = vec!["i32 -2147483648".to_string(), format!("i32 {word}")];
    let mut at: u64 = 8;
    for (k, ((v, ft), &o)) in values.iter().zip(&tys).zip(&offsets).enumerate() {
        if o > at {
            parts.push(format!("[{} x i8] zeroinitializer", o - at));
        }
        let lt = ty(ft)?;
        let text = match v {
            ConstValue::Lit(l) => format!("{lt} {}", lit(l)),
            ConstValue::Const(c) => match &m.consts.get(c).ok_or("an unknown constant")?.value {
                ConstValue::Lit(l) => format!("{lt} {}", lit(l)),
                _ => format!("ptr {}", const_symbol(*c)),
            },
            inner => {
                let sub = format!("{name}_{k}");
                const_global(m, out, entries, &sub, inner, ft)?;
                format!("ptr {sub}")
            }
        };
        parts.push(text);
        at = o + size(ft);
    }
    // After the fields, up to 16 for an `any`'s.
    let total = if after.is_empty() {
        total
    } else {
        total.max(at.next_multiple_of(8))
    };
    if total > at {
        parts.push(format!("[{} x i8] zeroinitializer", total - at));
    }
    parts.extend(after);
    let tys: Vec<&str> = parts
        .iter()
        .map(|p| p.rsplit_once(' ').map_or("", |(t, _)| t))
        .collect();
    // `[n x i8] zeroinitializer` splits at its last space too.
    let tys: Vec<String> = parts
        .iter()
        .zip(tys)
        .map(|(p, t)| match p.strip_suffix(" zeroinitializer") {
            Some(arr) => arr.to_string(),
            None => t.to_string(),
        })
        .collect();
    writeln!(
        out,
        "{name} = private unnamed_addr constant <{{ {} }}> <{{ {} }}>, align 8",
        tys.join(", "),
        parts.join(", ")
    )
    .unwrap();
    Ok(())
}

fn ctor_fields(m: &Module, c: CtorId) -> Result<Vec<Ty>, String> {
    m.data
        .get(&c.data)
        .and_then(|d| d.ctors.get(c.index as usize))
        .map(|c| c.fields.clone())
        .ok_or_else(|| format!("unknown constructor {c}"))
}

/// The entry function of `f`'s closures with `n` captures: it reads them
/// out of the closure and calls `f` with them, then the arguments.
fn entry(m: &Module, f: FuncId, n: usize, count: bool) -> Result<String, String> {
    let func = &m.funcs[&f];
    let sig = func.sig();
    let ret = ret_ty(&sig.results)?;
    let (offsets, _) = closure_layout(&sig.params[..n]);
    let mut s = String::new();
    let rest: Vec<String> = (sig.params[n..].iter().enumerate())
        .map(|(i, t)| ty(t).map(|t| format!("{t} %p{i}")))
        .collect::<Result<_, _>>()?;
    let mut params = vec!["ptr %c".to_string()];
    params.extend(rest.iter().cloned());
    writeln!(
        s,
        "\ndefine tailcc {ret} {}({}) {{",
        entry_symbol(f, n),
        params.join(", ")
    )
    .unwrap();
    let mut args = Vec::new();
    for (j, (t, o)) in sig.params[..n].iter().zip(&offsets).enumerate() {
        let lt = ty(t)?;
        writeln!(s, "  %q{j} = getelementptr i8, ptr %c, i64 {o}").unwrap();
        writeln!(s, "  %k{j} = load {lt}, ptr %q{j}").unwrap();
        // `apply` takes the closure, and the function owns its captures:
        // each gets a count, and the closure is dropped.
        if count && Kind::of(t).is_some() {
            writeln!(s, "  call tailcc void @conduit_dup(ptr %k{j})").unwrap();
        }
        args.push(format!("{lt} %k{j}"));
    }
    if count {
        writeln!(s, "  call tailcc void {}(ptr %c)", Kind::Closure.drop_fn()).unwrap();
    }
    args.extend(rest);
    let call = format!("tail call tailcc {ret} {}({})", symbol(f), args.join(", "));
    if ret == "void" {
        writeln!(s, "  {call}\n  ret void\n}}").unwrap();
    } else {
        writeln!(s, "  %r = {call}\n  ret {ret} %r\n}}").unwrap();
    }
    Ok(s)
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
    /// Slots beyond the variables', for loops that ops make.
    slots: Vec<&'static str>,
    /// The closure entry functions the function needs.
    entries: BTreeSet<(FuncId, usize)>,
    /// Whether to count, and the kinds of what it drops.
    counts: bool,
    kinds: BTreeSet<Kind>,
}

impl<'m> Lower<'m> {
    fn new(m: &'m Module, id: FuncId, f: &'m Func, counts: bool) -> Result<Lower<'m>, String> {
        Ok(Lower {
            m,
            id,
            f,
            defs: conduit_ir::tail::join_defs(&f.body, f.labels),
            ret: ret_ty(&f.results)?,
            out: String::new(),
            temps: 0,
            blocks: 0,
            slots: Vec::new(),
            entries: BTreeSet::new(),
            counts,
            kinds: BTreeSet::new(),
        })
    }

    fn func(&mut self) -> Result<String, String> {
        let params: Vec<String> = (self.f.params.iter())
            .map(|v| Ok(format!("{} %a{}", ty(self.f.var_ty(*v))?, v.0)))
            .collect::<Result<_, String>>()?;
        self.block(&self.f.body)?;
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
        for (i, t) in self.slots.iter().enumerate() {
            writeln!(head, "  %s{i} = alloca {t}").unwrap();
        }
        for v in &self.f.params {
            let t = ty(self.f.var_ty(*v))?;
            writeln!(head, "  store {t} %a{0}, ptr %v{0}", v.0).unwrap();
        }
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

    fn slot(&mut self, t: &'static str) -> String {
        self.slots.push(t);
        format!("%s{}", self.slots.len() - 1)
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

    fn bind(&mut self, binds: &[Var], value: &str) -> Result<(), String> {
        match binds.first() {
            Some(&b) => self.store(b, value),
            None => Ok(()),
        }
    }

    /// The address `off` bytes into object `p`; `off` is a constant or an
    /// `i64` value.
    fn at(&mut self, p: &str, off: &str) -> String {
        let q = self.temp();
        self.line(format!("{q} = getelementptr i8, ptr {p}, i64 {off}"));
        q
    }

    fn load_at(&mut self, t: &str, p: &str, off: &str) -> String {
        let q = self.at(p, off);
        let x = self.temp();
        self.line(format!("{x} = load {t}, ptr {q}"));
        x
    }

    fn store_at(&mut self, t: &str, v: &str, p: &str, off: &str) {
        let q = self.at(p, off);
        self.line(format!("store {t} {v}, ptr {q}"));
    }

    /// A new object of `size` bytes, an `i64` constant or value, with its
    /// count at 1 and `word` at 4. Trapping if there's no memory left.
    fn alloc(&mut self, size: &str, word: &str) -> String {
        let p = self.temp();
        // Zeroed, so that an array's elements that aren't set are null.
        self.line(format!("{p} = call ptr @calloc(i64 1, i64 {size})"));
        if self.counts {
            let (n, n1) = (self.temp(), self.temp());
            self.line(format!("{n} = load i64, ptr @conduit_live"));
            self.line(format!("{n1} = add i64 {n}, 1"));
            self.line(format!("store i64 {n1}, ptr @conduit_live"));
        }
        let null = self.temp();
        self.line(format!("{null} = icmp eq ptr {p}, null"));
        self.trap_if(&null);
        self.line(format!("store i32 1, ptr {p}"));
        self.store_at("i32", word, &p, "4");
        p
    }

    fn block(&mut self, b: &Block) -> Result<(), String> {
        conduit_ir::deep(|| {
            for (i, l) in b.lets.iter().enumerate() {
                let last = i + 1 == b.lets.len();
                let tail = last && conduit_ir::tail::returns(&l.binds, &b.tail, &self.defs);
                if tail
                    && let Op::Call(g, ..) = l.op
                    && self
                        .m
                        .funcs
                        .get(&g)
                        .is_some_and(|g| g.results == self.f.results)
                {
                    let args = self.args(&l.args)?;
                    return self.tail_call(&symbol(g), &args);
                }
                if tail
                    && l.op == Op::Apply
                    && let Ty::Fn(sig, _) = self.atom_ty(&l.args[0])
                    && sig.results == self.f.results
                {
                    let (fp, args) = self.apply_args(&l.args)?;
                    return self.tail_call(&fp, &args);
                }
                self.let_(&l.op, &l.args, &l.binds)?;
            }
            self.tail(&b.tail)
        })
    }

    /// A call that returns what it gives, which `tailcc` makes a tail call.
    fn tail_call(&mut self, callee: &str, args: &str) -> Result<(), String> {
        let ret = self.ret.clone();
        if ret == "void" {
            self.line(format!("tail call tailcc void {callee}({args})"));
            self.line("ret void");
        } else {
            let r = self.temp();
            self.line(format!("{r} = tail call tailcc {ret} {callee}({args})"));
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

    /// The entry function to call for `apply(c, args..)`, and the
    /// arguments: the closure, then `args`.
    fn apply_args(&mut self, args: &[Atom]) -> Result<(String, String), String> {
        let c = self.value(&args[0])?;
        let fp = self.load_at("ptr", &c, "8");
        let rest = self.args(&args[1..])?;
        let all = if rest.is_empty() {
            format!("ptr {c}")
        } else {
            format!("ptr {c}, {rest}")
        };
        Ok((fp, all))
    }

    /// The results of a call, of type `ret` from `results`, into `binds`.
    fn bind_results(&mut self, call: String, results: &[Ty], binds: &[Var]) -> Result<(), String> {
        match results {
            [] => self.line(call),
            [_] => {
                let r = self.temp();
                self.line(format!("{r} = {call}"));
                self.bind(binds, &r)?;
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

    /// The type of the array `a`'s elements.
    fn elem(&self, a: &Ty) -> Result<Ty, String> {
        match a {
            Ty::Array(e, _) => Ok((**e).clone()),
            other => Err(format!("an array op on a {other}")),
        }
    }

    /// The `i64` offset of element `i`, an `i32` value, in an array of
    /// elements of `stride` bytes.
    fn elem_offset(&mut self, i: &str, stride: u64) -> String {
        let w = self.temp();
        self.line(format!("{w} = zext i32 {i} to i64"));
        let m = self.temp();
        self.line(format!("{m} = mul i64 {w}, {stride}"));
        let o = self.temp();
        self.line(format!("{o} = add i64 {m}, 8"));
        o
    }

    /// The array's length, after trapping if `i` isn't an index of it.
    fn check_index(&mut self, a: &str, i: &str) {
        let len = self.load_at("i32", a, "4");
        let out = self.temp();
        self.line(format!("{out} = icmp uge i32 {i}, {len}"));
        self.trap_if(&out);
    }

    /// A new array of `n` elements, an `i32` value, trapping if `n` is
    /// negative.
    fn new_array(&mut self, n: &str, stride: u64) -> String {
        let neg = self.temp();
        self.line(format!("{neg} = icmp slt i32 {n}, 0"));
        self.trap_if(&neg);
        let bytes = self.elem_offset(n, stride);
        self.alloc(&bytes, n)
    }

    fn let_(&mut self, op: &Op, args: &[Atom], binds: &[Var]) -> Result<(), String> {
        match op {
            Op::Prim(p) => {
                let r = self.prim(p, args)?;
                self.bind(binds, &r)?;
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
            Op::Closure(f, ..) => {
                let tys: Vec<Ty> = args.iter().map(|a| self.atom_ty(a)).collect();
                let (offsets, total) = closure_layout(&tys);
                self.entries.insert((*f, args.len()));
                let p = self.alloc(&total.to_string(), "0");
                self.store_at("ptr", &entry_symbol(*f, args.len()), &p, "8");
                let free = if self.counts {
                    free_symbol(*f, args.len())
                } else {
                    "null".into()
                };
                self.store_at("ptr", &free, &p, "16");
                for ((a, t), o) in args.iter().zip(&tys).zip(offsets) {
                    let v = self.value(a)?;
                    self.store_at(ty(t)?, &v, &p, &o.to_string());
                }
                self.bind(binds, &p)?;
            }
            Op::Apply => {
                let Ty::Fn(sig, _) = self.atom_ty(&args[0]) else {
                    return Err("apply of something that isn't a closure".into());
                };
                let ret = ret_ty(&sig.results)?;
                let (fp, all) = self.apply_args(args)?;
                let call = format!("call tailcc {ret} {fp}({all})");
                self.bind_results(call, &sig.results, binds)?;
            }
            Op::New(c) | Op::Reuse(c) => {
                let fields = if matches!(op, Op::Reuse(_)) {
                    &args[1..]
                } else {
                    args
                };
                let tys = ctor_fields(self.m, *c)?;
                let (offsets, total) = layout(&tys);
                let p = if matches!(op, Op::Reuse(_)) && self.counts {
                    self.reuse(&args[0], &total.to_string(), &c.index.to_string())?
                } else {
                    // Without counts, a reuse token holds no memory.
                    self.alloc(&total.to_string(), &c.index.to_string())
                };
                for ((a, t), o) in fields.iter().zip(&tys).zip(offsets) {
                    let v = self.value(a)?;
                    self.store_at(ty(t)?, &v, &p, &o.to_string());
                }
                self.bind(binds, &p)?;
            }
            Op::Const(c) => {
                let decl = self
                    .m
                    .consts
                    .get(c)
                    .ok_or_else(|| format!("unknown constant {c}"))?;
                let v = match &decl.value {
                    ConstValue::Lit(l) => lit(l),
                    _ => const_symbol(*c),
                };
                self.bind(binds, &v)?;
            }
            Op::Get(c, i) | Op::Set(c, i) => {
                let tys = ctor_fields(self.m, *c)?;
                let (offsets, _) = layout(&tys);
                let (t, o) = (ty(&tys[*i as usize])?, offsets[*i as usize].to_string());
                let p = self.value(&args[0])?;
                if matches!(op, Op::Get(..)) {
                    let x = self.load_at(t, &p, &o);
                    self.bind(binds, &x)?;
                } else {
                    let v = self.value(&args[1])?;
                    self.replace_at(&tys[*i as usize], &v, &p, &o)?;
                }
            }
            Op::ArrayNew => {
                let e = self.elem(self.f.var_ty(binds[0]))?;
                let n = args.len().to_string();
                let p = self.new_array(&n, size(&e));
                for (k, a) in args.iter().enumerate() {
                    let v = self.value(a)?;
                    let o = 8 + k as u64 * size(&e);
                    self.store_at(ty(&e)?, &v, &p, &o.to_string());
                }
                self.bind(binds, &p)?;
            }
            Op::ArrayFill | Op::ArrayAlloc => {
                let e = self.elem(self.f.var_ty(binds[0]))?;
                let n = self.value(&args[0])?;
                let p = self.new_array(&n, size(&e));
                if *op == Op::ArrayFill {
                    let v = self.value(&args[1])?;
                    self.fill(&p, &n, &v, &e)?;
                    // It takes one reference, and the array holds `n`.
                    if self.counts && Kind::of(&e).is_some() {
                        self.add_count(&v, &n, &e);
                    }
                }
                self.bind(binds, &p)?;
            }
            Op::ArrayGet | Op::ArraySet => {
                let e = self.elem(&self.atom_ty(&args[0]))?;
                let a = self.value(&args[0])?;
                let i = self.value(&args[1])?;
                self.check_index(&a, &i);
                let o = self.elem_offset(&i, size(&e));
                if *op == Op::ArrayGet {
                    let x = self.load_at(ty(&e)?, &a, &o);
                    self.bind(binds, &x)?;
                } else {
                    let v = self.value(&args[2])?;
                    self.replace_at(&e, &v, &a, &o)?;
                }
            }
            Op::ArrayLen => {
                let a = self.value(&args[0])?;
                let n = self.load_at("i32", &a, "4");
                self.bind(binds, &n)?;
            }
            Op::ArrayCopy => self.array_copy(args)?,
            Op::ArrayClone => {
                let e = self.elem(&self.atom_ty(&args[0]))?;
                let a = self.value(&args[0])?;
                let n = self.load_at("i32", &a, "4");
                let p = self.new_array(&n, size(&e));
                let bytes = self.elem_offset(&n, size(&e));
                let elems = self.temp();
                self.line(format!("{elems} = sub i64 {bytes}, 8"));
                let (to, from) = (self.at(&p, "8"), self.at(&a, "8"));
                self.line(format!(
                    "call void @llvm.memmove.p0.p0.i64(ptr {to}, ptr {from}, i64 {elems}, i1 false)"
                ));
                // The clone shares the elements.
                if self.counts && Kind::of(&e).is_some() {
                    self.each(&p, "0", &n, "@conduit_dup");
                }
                self.bind(binds, &p)?;
            }
            // The same array, now `imm`.
            Op::ArrayFreeze => {
                let a = self.value(&args[0])?;
                self.bind(binds, &a)?;
            }
            Op::RefNew => {
                let t = self.atom_ty(&args[0]);
                let p = self.alloc("16", "0");
                let v = self.value(&args[0])?;
                self.store_at(ty(&t)?, &v, &p, "8");
                self.bind(binds, &p)?;
            }
            // An `any` holds the drop of what it holds, if it's counted.
            Op::Box(t) => {
                let p = self.alloc("24", "0");
                let v = self.value(&args[0])?;
                self.store_at(ty(t)?, &v, &p, "8");
                let drop = match Kind::of(t) {
                    Some(k) if self.counts => {
                        self.kinds.insert(k.clone());
                        k.drop_fn()
                    }
                    _ => "null".into(),
                };
                self.store_at("ptr", &drop, &p, "16");
                self.bind(binds, &p)?;
            }
            Op::RefGet | Op::Unbox(_) => {
                let t = self.f.var_ty(binds[0]).clone();
                let p = self.value(&args[0])?;
                let x = self.load_at(ty(&t)?, &p, "8");
                self.bind(binds, &x)?;
            }
            Op::RefSet => {
                let Ty::Ref(t) = self.atom_ty(&args[0]) else {
                    return Err("ref_set of something that isn't a ref".into());
                };
                let p = self.value(&args[0])?;
                let v = self.value(&args[1])?;
                self.replace_at(&t, &v, &p, "8")?;
            }
            // Nothing to store: whatever the slot holds is what it gives,
            // as nothing may use it, counting included. But a reuse token
            // that's undefined holds no memory, which `reuse` checks for.
            Op::Undefined => {
                if let Some(&b) = binds.first()
                    && matches!(self.f.var_ty(b), Ty::Reuse(_))
                {
                    self.store(b, "null")?;
                }
            }
            // Without counts, nothing is freed: counting does nothing, no
            // object is unique, and a reuse token holds no memory.
            // Gives up a reference without counting.
            Op::Forget => {}
            Op::Dup | Op::Drop | Op::Free | Op::DecRef if !self.counts => {}
            Op::IsUnique if !self.counts => self.bind(binds, "0")?,
            Op::DropReuse | Op::ReuseToken if !self.counts => self.bind(binds, "null")?,
            Op::Dup | Op::Drop => {
                let t = self.atom_ty(&args[0]);
                let Some(k) = Kind::of(&t) else {
                    return Ok(());
                };
                let x = self.value(&args[0])?;
                let f = if *op == Op::Dup {
                    "@conduit_dup".to_string()
                } else {
                    k.drop_fn()
                };
                self.kinds.insert(k);
                self.line(format!("call tailcc void {f}(ptr {x})"));
            }
            Op::IsUnique => {
                let x = self.value(&args[0])?;
                let r = self.temp();
                self.line(format!("{r} = call tailcc i32 @conduit_is_unique(ptr {x})"));
                self.bind(binds, &r)?;
            }
            Op::Free => {
                let x = self.value(&args[0])?;
                self.line(format!("call tailcc void @conduit_release(ptr {x})"));
            }
            Op::DecRef => {
                let x = self.value(&args[0])?;
                self.line(format!("call tailcc void @conduit_decref(ptr {x})"));
            }
            // At 0, what it held is dropped and its memory is the token.
            Op::DropReuse => {
                let Ty::Data(d, ..) = self.atom_ty(&args[0]) else {
                    return Err("drop_reuse of something that isn't data".into());
                };
                self.kinds.insert(Kind::Data(d));
                let x = self.value(&args[0])?;
                let last = self.temp();
                self.line(format!("{last} = call tailcc i1 @conduit_last(ptr {x})"));
                let (free, done) = (self.new_block(), self.new_block());
                self.line(format!("br i1 {last}, label %{free}, label %{done}"));
                self.start(&free);
                self.line(format!(
                    "call tailcc void @conduit_children.data{}(ptr {x})",
                    d.0
                ));
                self.line(format!("br label %{done}"));
                self.start(&done);
                let token = self.temp();
                self.line(format!("{token} = select i1 {last}, ptr {x}, ptr null"));
                self.bind(binds, &token)?;
            }
            Op::ReuseToken => {
                let x = self.value(&args[0])?;
                self.bind(binds, &x)?;
            }
            other => return Err(format!("{other:?} isn't supported yet")),
        }
        Ok(())
    }

    /// Stores `v`, of type `t`, at `off` in `p`, and drops what it
    /// replaces, when counting.
    fn replace_at(&mut self, t: &Ty, v: &str, p: &str, off: &str) -> Result<(), String> {
        let lt = ty(t)?;
        match Kind::of(t) {
            Some(k) if self.counts => {
                let old = self.load_at(lt, p, off);
                self.store_at(lt, v, p, off);
                self.line(format!("call tailcc void {}(ptr {old})", k.drop_fn()));
                self.kinds.insert(k);
            }
            _ => self.store_at(lt, v, p, off),
        }
        Ok(())
    }

    /// Gives `v` the `n - 1` more references an array of `n` copies of it
    /// holds, or drops it if `n` is 0.
    fn add_count(&mut self, v: &str, n: &str, e: &Ty) {
        let k = Kind::of(e).expect("a counted element");
        let none = self.temp();
        self.line(format!("{none} = icmp eq i32 {n}, 0"));
        let (zero, some, done) = (self.new_block(), self.new_block(), self.new_block());
        self.line(format!("br i1 {none}, label %{zero}, label %{some}"));
        self.start(&zero);
        self.line(format!("call tailcc void {}(ptr {v})", k.drop_fn()));
        self.line(format!("br label %{done}"));
        self.start(&some);
        let more = self.temp();
        self.line(format!("{more} = sub i32 {n}, 1"));
        self.line(format!(
            "call tailcc void @conduit_add(ptr {v}, i32 {more})"
        ));
        self.line(format!("br label %{done}"));
        self.start(&done);
        self.kinds.insert(k);
    }

    /// Calls `f` on each of the `n` elements of array `a` from `start`, all
    /// `i32` values, which are objects.
    fn each(&mut self, a: &str, start: &str, n: &str, f: &str) {
        let i = self.slot("i32");
        self.line(format!("store i32 0, ptr {i}"));
        let (head, body, done) = (self.new_block(), self.new_block(), self.new_block());
        self.line(format!("br label %{head}"));
        self.start(&head);
        let k = self.temp();
        self.line(format!("{k} = load i32, ptr {i}"));
        let more = self.temp();
        self.line(format!("{more} = icmp slt i32 {k}, {n}"));
        self.line(format!("br i1 {more}, label %{body}, label %{done}"));
        self.start(&body);
        let at = self.temp();
        self.line(format!("{at} = add i32 {start}, {k}"));
        let o = self.elem_offset(&at, 8);
        let x = self.load_at("ptr", a, &o);
        self.line(format!("call tailcc void {f}(ptr {x})"));
        let k1 = self.temp();
        self.line(format!("{k1} = add i32 {k}, 1"));
        self.line(format!("store i32 {k1}, ptr {i}"));
        self.line(format!("br label %{head}"));
        self.start(&done);
    }

    /// An object of `size` bytes in the memory of `token`, if it holds
    /// some, or a new one: either way with its count at 1 and `word` at 4.
    fn reuse(&mut self, token: &Atom, size: &str, word: &str) -> Result<String, String> {
        let t = self.value(token)?;
        let out = self.slot("ptr");
        let none = self.temp();
        self.line(format!("{none} = icmp eq ptr {t}, null"));
        let (fresh, reused, done) = (self.new_block(), self.new_block(), self.new_block());
        self.line(format!("br i1 {none}, label %{fresh}, label %{reused}"));
        self.start(&fresh);
        let p = self.alloc(size, word);
        self.line(format!("store ptr {p}, ptr {out}"));
        self.line(format!("br label %{done}"));
        self.start(&reused);
        self.line(format!("store i32 1, ptr {t}"));
        self.store_at("i32", word, &t, "4");
        self.line(format!("store ptr {t}, ptr {out}"));
        self.line(format!("br label %{done}"));
        self.start(&done);
        let r = self.temp();
        self.line(format!("{r} = load ptr, ptr {out}"));
        Ok(r)
    }

    /// Sets each of the `n` elements of array `p` to `v`.
    fn fill(&mut self, p: &str, n: &str, v: &str, e: &Ty) -> Result<(), String> {
        let i = self.slot("i32");
        self.line(format!("store i32 0, ptr {i}"));
        let (head, body, done) = (self.new_block(), self.new_block(), self.new_block());
        self.line(format!("br label %{head}"));
        self.start(&head);
        let k = self.temp();
        self.line(format!("{k} = load i32, ptr {i}"));
        let more = self.temp();
        self.line(format!("{more} = icmp slt i32 {k}, {n}"));
        self.line(format!("br i1 {more}, label %{body}, label %{done}"));
        self.start(&body);
        let o = self.elem_offset(&k, size(e));
        self.store_at(ty(e)?, v, p, &o);
        let k1 = self.temp();
        self.line(format!("{k1} = add i32 {k}, 1"));
        self.line(format!("store i32 {k1}, ptr {i}"));
        self.line(format!("br label %{head}"));
        self.start(&done);
        Ok(())
    }

    /// `array_copy(dst, dst_start, src, src_start, count)`: as `memmove`,
    /// trapping unless both ranges fit, read as unsigned.
    fn array_copy(&mut self, args: &[Atom]) -> Result<(), String> {
        let e = self.elem(&self.atom_ty(&args[0]))?;
        let stride = size(&e);
        let vals: Vec<String> = args
            .iter()
            .map(|a| self.value(a))
            .collect::<Result<_, _>>()?;
        let (dst, ds, src, ss, n) = (&vals[0], &vals[1], &vals[2], &vals[3], &vals[4]);
        let n64 = self.temp();
        self.line(format!("{n64} = zext i32 {n} to i64"));
        for (a, start) in [(dst, ds), (src, ss)] {
            let len = self.load_at("i32", a, "4");
            let (s64, l64, end) = (self.temp(), self.temp(), self.temp());
            self.line(format!("{s64} = zext i32 {start} to i64"));
            self.line(format!("{l64} = zext i32 {len} to i64"));
            self.line(format!("{end} = add i64 {s64}, {n64}"));
            let out = self.temp();
            self.line(format!("{out} = icmp ugt i64 {end}, {l64}"));
            self.trap_if(&out);
        }
        // The copies are references more, and what they overwrite one less.
        if self.counts
            && let Some(k) = Kind::of(&e)
        {
            self.each(src, ss, n, "@conduit_dup");
            self.each(dst, ds, n, &k.drop_fn());
            self.kinds.insert(k);
        }
        let to = self.elem_offset(ds, stride);
        let from = self.elem_offset(ss, stride);
        let (to, from) = (self.at(dst, &to), self.at(src, &from));
        let bytes = self.temp();
        self.line(format!("{bytes} = mul i64 {n64}, {stride}"));
        self.line(format!(
            "call void @llvm.memmove.p0.p0.i64(ptr {to}, ptr {from}, i64 {bytes}, i1 false)"
        ));
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
                let st = self.atom_ty(scrut);
                let s = self.value(scrut)?;
                // A case on data is on its constructor, at 4.
                let (t, s) = if ty(&st)? == "ptr" {
                    ("i32", self.load_at("i32", &s, "4"))
                } else {
                    (ty(&st)?, s)
                };
                let names: Vec<String> = arms.iter().map(|_| self.new_block()).collect();
                let other = self.new_block();
                let cases: Vec<String> = (arms.iter().zip(&names))
                    .map(|(arm, name)| {
                        let n = match arm.tag {
                            Tag::Int(n) => n,
                            Tag::Ctor(c) => c.index as i64,
                        };
                        format!("{t} {n}, label %{name}")
                    })
                    .collect();
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
            (true, false) => return Ok(self.float_to_int(from, to, sign, x)),
        };
        let r = self.temp();
        self.line(format!("{r} = {op} {f} {x} to {t}"));
        Ok(r)
    }
}

impl Lower<'_> {
    /// A float converted to an integer, rounding toward 0, which traps if
    /// it's NaN or out of the integer's range, as in Wasm. It converts when
    /// it's above the smallest integer minus one and below the largest plus
    /// one, or from the smallest itself, a power of 2, where the smallest
    /// minus one isn't a value of the float type. NaN fails every ordered
    /// comparison, so it traps too.
    fn float_to_int(&mut self, from: &Ty, to: &Ty, sign: Sign, x: &str) -> String {
        let (f, t) = (ty(from).unwrap(), ty(to).unwrap());
        let bits = int_bits(to) as i32;
        let as_from = |v: f64| -> (String, bool) {
            let exact = if *from == Ty::F32 {
                f64::from(v as f32) == v
            } else {
                true
            };
            (
                lit(&if *from == Ty::F32 {
                    Lit::f32(v as f32)
                } else {
                    Lit::f64(v)
                }),
                exact,
            )
        };
        let (lo_test, hi) = match sign {
            Sign::Signed => {
                let min = -(2f64.powi(bits - 1));
                let (lo, exact) = as_from(min - 1.0);
                let lo_test = if exact && (min - 1.0) != min {
                    format!("fcmp ogt {f} {x}, {lo}")
                } else {
                    format!("fcmp oge {f} {x}, {}", as_from(min).0)
                };
                (lo_test, as_from(2f64.powi(bits - 1)).0)
            }
            Sign::Unsigned => (
                format!("fcmp ogt {f} {x}, {}", as_from(-1.0).0),
                as_from(2f64.powi(bits)).0,
            ),
        };
        let above = self.temp();
        self.line(format!("{above} = {lo_test}"));
        let below = self.temp();
        self.line(format!("{below} = fcmp olt {f} {x}, {hi}"));
        let ok = self.temp();
        self.line(format!("{ok} = and i1 {above}, {below}"));
        let bad = self.temp();
        self.line(format!("{bad} = xor i1 {ok}, true"));
        self.trap_if(&bad);
        let op = if sign == Sign::Signed {
            "fptosi"
        } else {
            "fptoui"
        };
        let r = self.temp();
        self.line(format!("{r} = {op} {f} {x} to {t}"));
        r
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
