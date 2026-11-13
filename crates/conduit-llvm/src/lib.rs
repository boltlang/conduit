//! Lowers Conduit IR to LLVM IR, as text that `clang` or `llc` compile.
//!
//! An experiment, and a first step towards a native backend: it handles
//! functions, calls, join points, `case`, the integer and float ops,
//! objects (data, arrays, refs, `any`, closures), constants, host values
//! and functions, freeing by reference counting, and handlers whose
//! clauses are tail-resumptive or aborting (see `handlers`), and says what
//! it doesn't handle yet: other handlers, which lowering effects removes
//! first. See `docs/backends/llvm.md`.
//!
//! The lowering builds an [`ir::Module`], which printing makes text.
//!
//! - **Functions** are `tailcc`, LLVM's calling convention that guarantees
//!   a call marked `tail` right before a `ret` is a tail call, as Conduit
//!   promises (see `conduit_ir::tail`). Several results are a struct.
//! - **Variables** each get an `alloca` in the entry block, which every
//!   binding stores to and every use loads from, so join point parameters
//!   need no phi nodes: LLVM's `mem2reg` makes SSA of them again.
//! - **Join points** are basic blocks, and a jump stores its arguments in
//!   the parameters' slots and branches. A `case` is a `switch`.
//! - **Objects** are `ptr`s to memory from free lists by size (see
//!   `heap`), laid out as in the
//!   Wasm backend: a header word for the count, then a word with the
//!   constructor, the length or the kind, then the fields from offset 8,
//!   each at a multiple of its size. A closure holds its entry function's
//!   address at 8, its descriptor at 16 (the functions that free it and
//!   visit what it holds), then its captures; an `any` holds its value at
//!   8 and the descriptor of what it holds at 16; a host value's box holds
//!   the host's pointer at 8 and its `conduit_host_ops` at 16. With
//!   [`Options::counts`], objects are counted and freed (see `counts`);
//!   without, nothing is freed, and the ops that count do nothing, as in
//!   the Wasm backend.
//! - **Ops** follow Conduit's semantics where LLVM's differ: integer
//!   division by 0 gives 0 and the remainder the dividend, signed
//!   division that overflows traps, shift amounts are taken modulo the
//!   width, `min` and `max` give NaN if either operand is NaN, and array
//!   accesses out of bounds trap.

mod counts;
mod debug;
mod handlers;
mod heap;
pub mod ir;
pub mod native;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

pub use debug::{Location, Sources};

use counts::Kind;
use ir::{
    BinOp, Call, CallConv, CastOp, Declare, FloatPred, FnBuilder, Global, IntPred, Item, Type,
    Typed, Value,
};

use conduit_ir::{
    Abi, Atom, Block, ConstId, ConstValue, CtorId, ExternId, Func, FuncId, JoinDef, Label, Link,
    Lit, Module, Op, Prim, Sign, SourceId, Tag, Tail, Ty, Var,
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
    /// With `counts`, whether cycles are collected too, by trial deletion,
    /// as in the Wasm backend: when 256 possible roots are buffered, and
    /// when the C function `conduit_collect` is called. Without it, what's
    /// left in a cycle leaks.
    pub cycles: bool,
}

fn fn_name(f: FuncId) -> String {
    format!("fn{}", f.0)
}

/// The LLVM symbol of function `f`.
pub fn symbol(f: FuncId) -> Value {
    Value::Global(fn_name(f))
}

/// The entry function of `f`'s closures with `n` captures.
fn entry_name(f: FuncId, n: usize) -> String {
    format!("fn{}_entry{n}", f.0)
}

/// The function that frees a closure of `f` with `n` captures.
fn free_name(f: FuncId, n: usize) -> String {
    format!("fn{}_free{n}", f.0)
}

/// The collector's visit function of a closure of `f` with `n` captures.
fn visit_name(f: FuncId, n: usize) -> String {
    format!("fn{}_visit{n}", f.0)
}

/// What a closure of `f` with `n` captures holds at 16, with counts: its
/// free and visit functions.
fn desc_name(f: FuncId, n: usize) -> String {
    format!("fn{}_desc{n}", f.0)
}

fn const_name(c: ConstId) -> String {
    format!("const{}", c.0)
}

/// The LLVM type of a value of type `ty`, if it's one this backend
/// handles yet: an object is a `ptr`.
pub fn ty(ty: &Ty) -> Result<Type, String> {
    Ok(match ty {
        Ty::I8 => Type::I8,
        Ty::I16 => Type::I16,
        Ty::I32 => Type::I32,
        Ty::I64 => Type::I64,
        Ty::F32 => Type::Float,
        Ty::F64 => Type::Double,
        // A host value is its box, or null.
        Ty::Data(..)
        | Ty::Array(..)
        | Ty::Fn(..)
        | Ty::Ref(_)
        | Ty::Any(_)
        | Ty::Reuse(_)
        | Ty::Host
        | Ty::NullableHost => Type::Ptr,
        other => return Err(format!("values of type {other} aren't supported yet")),
    })
}

/// A new object of `size` bytes, an `i64` constant or value, with its
/// count at 1 and `word` at 4, counted live with `counts`. Trapping if
/// there's no memory left.
fn alloc(b: &mut FnBuilder, counts: bool, size: impl Into<Value>, word: impl Into<Value>) -> Value {
    // Not zeroed: the caller sets what's read (see `heap`).
    let args = vec![Type::I64.val(size)];
    let p = b.call_value(Call::c(Type::Ptr, Value::global("conduit_alloc"), args));
    if counts {
        let live = Value::global("conduit_live");
        let n = b.load(Type::I64, &live);
        let n1 = b.bin(BinOp::Add, Type::I64, n, 1);
        b.store(Type::I64, n1, &live);
    }
    let null = b.icmp(IntPred::Eq, Type::Ptr, &p, Value::Null);
    trap_if(b, null);
    b.store(Type::I32, 1, &p);
    b.store_at(Type::I32, word, &p, 4);
    p
}

/// Traps if `cond`, an `i1`, is true.
fn trap_if(b: &mut FnBuilder, cond: Value) {
    let (trapped, ok) = (b.new_block(), b.new_block());
    b.cond_br(cond, &trapped, &ok);
    b.start(&trapped);
    trap(b);
    b.start(&ok);
}

/// What `then` gives if `cond`, an `i1`, is true, else null: a pointer,
/// which `then` makes in a block of its own.
fn ptr_if(b: &mut FnBuilder, cond: Value, then: impl FnOnce(&mut FnBuilder) -> Value) -> Value {
    let slot = b.alloca(Type::Ptr);
    b.store(Type::Ptr, Value::Null, &slot);
    let (yes, done) = (b.new_block(), b.new_block());
    b.cond_br(cond, &yes, &done);
    b.start(&yes);
    let v = then(b);
    b.store(Type::Ptr, v, &slot);
    b.br(&done);
    b.start(&done);
    b.load(Type::Ptr, &slot)
}

/// A host value the host gives Conduit, `value` with its `ops`, in a new
/// box of 24 bytes: the header, then `value` at 8 and `ops` at 16. The box
/// owns the host's reference, which dropping it releases. A null `value`
/// is a null box if `nullable`, and traps if not, as the type promised a
/// value.
fn host_in(b: &mut FnBuilder, counts: bool, value: Value, ops: Value, nullable: bool) -> Value {
    let null = b.icmp(IntPred::Eq, Type::Ptr, &value, Value::Null);
    let some = b.icmp(IntPred::Ne, Type::Ptr, &value, Value::Null);
    let boxed = |b: &mut FnBuilder| {
        let p = alloc(b, counts, 24, 0);
        b.store_at(Type::Ptr, value, &p, 8);
        b.store_at(Type::Ptr, ops, &p, 16);
        p
    };
    if nullable {
        ptr_if(b, some, boxed)
    } else {
        trap_if(b, null);
        boxed(b)
    }
}

/// The host's value in box `p`, or null for a null box, which only a
/// `nullable host` is.
fn host_out(b: &mut FnBuilder, p: &Value, nullable: bool) -> Value {
    if nullable {
        let some = b.icmp(IntPred::Ne, Type::Ptr, p, Value::Null);
        let p = p.clone();
        ptr_if(b, some, |b| b.load_at(Type::Ptr, p, 8))
    } else {
        b.load_at(Type::Ptr, p, 8)
    }
}

/// The C signature of extern `ext`: its return type and its parameters'.
/// It returns its first result, and takes a pointer for each other one,
/// after its arguments, to write it to, which C returns the same way on
/// every target (see [`c_results`]). Under the flat ABI, which natively is
/// the C ABI (see [`Abi::Flat`]): an
/// array is a pointer to its elements and an `i32` length, or the pointer
/// alone if it's `#[addr]`; a list of byte buffers is a pointer to a
/// `struct iovec` for each and how many there are; an `i8` or `i16` is
/// zero-extended to an `i32`, as Wasm passes it, which C's ABI takes for
/// a narrower parameter too; and each result after the first is a pointer
/// to where the callee writes it.
pub fn extern_sig(ext: &conduit_ir::Extern) -> Result<(Type, Vec<Type>), String> {
    let sig = &ext.sig;
    if ext.abi != Abi::Flat {
        let (ret, outs) = c_results(&sig.results)?;
        let mut params: Vec<Type> = sig.params.iter().map(ty).collect::<Result<_, _>>()?;
        params.extend(outs.iter().map(|_| Type::Ptr));
        return Ok((ret, params));
    }
    let mut params = Vec::new();
    for (i, t) in sig.params.iter().enumerate() {
        match t {
            Ty::Array(..) if ext.addr(i) => params.push(Type::Ptr),
            Ty::Array(..) => params.extend([Type::Ptr, Type::I32]),
            Ty::I8 | Ty::I16 => params.push(Type::I32),
            t => params.push(ty(t)?),
        }
    }
    params.extend(sig.results.iter().skip(1).map(|_| Type::Ptr));
    Ok((ret_ty(&sig.results[..sig.results.len().min(1)])?, params))
}

/// A host value as C passes it into Conduit, from an extern or to an
/// export: `struct conduit_host { void *value; const struct
/// conduit_host_ops *ops; }`, whose `ops` are `{ void (*retain)(void *);
/// void (*release)(void *); }`. A null is a null `value`. Out of Conduit,
/// to an extern, it's the `value` alone, which the host borrows for the
/// call.
fn host_pair() -> Type {
    Type::Struct(vec![Type::Ptr, Type::Ptr])
}

/// The C type of a result of type `t` that C gives Conduit, from an
/// extern, or Conduit gives C, from an export: a host value is a
/// [`host_pair`].
fn c_ty(t: &Ty) -> Result<Type, String> {
    if t.is_host() { Ok(host_pair()) } else { ty(t) }
}

/// How a C function giving `results` to Conduit, or an export giving them
/// to C, gives them: what it returns, the first one or `void`, and the
/// types of the others, which it writes through a pointer for each, given
/// after its arguments. A struct of them isn't returned: C's ABI returns
/// a struct in ways that depend on its size and on the target, which LLVM
/// leaves to its frontends, and out-pointers are the same everywhere, as
/// in the flat ABI.
fn c_results(results: &[Ty]) -> Result<(Type, Vec<Type>), String> {
    let ret = match results.first() {
        Some(t) => c_ty(t)?,
        None => Type::Void,
    };
    let outs = results.iter().skip(1).map(c_ty).collect::<Result<_, _>>()?;
    Ok((ret, outs))
}

/// What a function giving `results` returns: `void`, the one result, or a
/// struct of them.
pub fn ret_ty(results: &[Ty]) -> Result<Type, String> {
    Ok(match results {
        [] => Type::Void,
        [one] => ty(one)?,
        many => Type::Struct(many.iter().map(ty).collect::<Result<_, _>>()?),
    })
}

/// A literal as an LLVM constant. A float is the double that has the
/// same value, which is how LLVM writes a `float` too.
pub fn lit(l: &Lit) -> Value {
    match *l {
        Lit::I8(n) => Value::Int(n.into()),
        Lit::I16(n) => Value::Int(n.into()),
        Lit::I32(n) => Value::Int(n.into()),
        Lit::I64(n) => Value::Int(n),
        Lit::F32(bits) => Value::Float(f64::from(f32::from_bits(bits)).to_bits()),
        Lit::F64(bits) => Value::Float(bits),
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

/// Calls `tailcc` function `f`, which gives nothing.
fn call_void(b: &mut FnBuilder, f: Value, args: Vec<Typed>) {
    b.call(Call::tailcc(Type::Void, f, args));
}

/// Calls `llvm.trap`, which ends the block.
fn trap(b: &mut FnBuilder) {
    b.call(Call::c(Type::Void, Value::global("llvm.trap"), vec![]));
    b.unreachable();
}

/// The `i64` offset of element `i`, an `i32` value, in an array of
/// elements of `stride` bytes.
fn elem_offset(b: &mut FnBuilder, i: impl Into<Value>, stride: u64) -> Value {
    let w = b.cast(CastOp::ZExt, Type::I32, i, Type::I64);
    let m = b.bin(BinOp::Mul, Type::I64, w, stride);
    b.bin(BinOp::Add, Type::I64, m, 8)
}

/// A loop of `body` on each `i32` from 0 to `n`, an `i32` value, with the
/// counter in a slot; after it, the builder is in the block after the loop.
fn count_loop(b: &mut FnBuilder, n: &Value, body: impl FnOnce(&mut FnBuilder, &Value)) {
    let i = b.alloca(Type::I32);
    b.store(Type::I32, 0, &i);
    let (head, next, done) = (b.new_block(), b.new_block(), b.new_block());
    b.br(&head);
    b.start(&head);
    let k = b.load(Type::I32, &i);
    let more = b.icmp(IntPred::Slt, Type::I32, &k, n);
    b.cond_br(more, &next, &done);
    b.start(&next);
    body(b, &k);
    let k1 = b.bin(BinOp::Add, Type::I32, &k, 1);
    b.store(Type::I32, k1, &i);
    b.br(&head);
    b.start(&done);
}

/// The module as LLVM IR text, or what it has that isn't supported yet,
/// with the default [`Options`]: nothing is freed.
pub fn emit_module(m: &Module) -> Result<String, Error> {
    emit_module_with(m, &Options::default())
}

/// The module as LLVM IR text, or what it has that isn't supported yet.
/// See [`lower_module_with`].
pub fn emit_module_with(m: &Module, options: &Options) -> Result<String, Error> {
    lower_module_with(m, options).map(|m| m.to_string())
}

/// [`emit_module_with`], with debug info from where `sources` says the
/// module's sources are: see [`lower_module_with_sources`].
pub fn emit_module_with_sources(
    m: &Module,
    options: &Options,
    sources: &Sources,
) -> Result<String, Error> {
    lower_module_with_sources(m, options, sources).map(|m| m.to_string())
}

/// The module as LLVM IR, or what it has that isn't supported yet. Each
/// function `f` is `@fn<f>`, `tailcc`, and each `#[export(c, symbol =
/// "s")]` gets a C-callable `@s` that calls it. Externs must be
/// `#[link(c, symbol = "..")]`.
pub fn lower_module_with(m: &Module, options: &Options) -> Result<ir::Module, Error> {
    lower_module_with_sources(m, options, &Sources::default())
}

/// [`lower_module_with`], with DWARF debug info, if `sources` has any
/// locations: each function with a source in its body is a
/// `DISubprogram`, and each instruction lowered from a `let` or `jump`
/// with a source has its location, so that debuggers and profilers show
/// where in the frontend's program code comes from.
pub fn lower_module_with_sources(
    m: &Module,
    options: &Options,
    sources: &Sources,
) -> Result<ir::Module, Error> {
    let err = |func: Option<FuncId>, message: String| Error { func, message };
    let debug = debug::Debug::new(sources).map(RefCell::new);
    let (m, hs) = handlers::plain_clauses(m).map_err(|message| err(None, message))?;
    let m = &*m;
    let mut out = ir::Module::default();
    out.push(Item::Comment(
        "Lowered from Conduit IR by conduit-llvm.".into(),
    ));
    out.push(Declare {
        attrs: vec!["noreturn", "nounwind"],
        ..Declare::new(Type::Void, "llvm.trap", vec![])
    });
    // For the landing pads of what an abort or an exception from the host
    // unwinds.
    out.push(Declare {
        variadic: true,
        ..Declare::new(Type::I32, handlers::PERSONALITY, vec![])
    });
    out.push(Declare::new(
        Type::Ptr,
        "calloc",
        vec![Type::I64, Type::I64],
    ));
    out.push(Declare::new(Type::Void, "free", vec![Type::Ptr]));
    out.push(Declare::new(
        Type::Void,
        "llvm.memset.p0.i64",
        vec![Type::Ptr, Type::I8, Type::I64, Type::I1],
    ));
    out.push(Item::Text(heap::runtime()));
    out.push(Declare::new(
        Type::Void,
        "llvm.memmove.p0.p0.i64",
        vec![Type::Ptr, Type::Ptr, Type::I64, Type::I1],
    ));
    for (t, s) in [(Type::Float, "f32"), (Type::Double, "f64")] {
        for op in ["fabs", "sqrt"] {
            out.push(Declare::new(
                t.clone(),
                format!("llvm.{op}.{s}"),
                vec![t.clone()],
            ));
        }
        for op in ["minimum", "maximum"] {
            let params = vec![t.clone(), t.clone()];
            out.push(Declare::new(t.clone(), format!("llvm.{op}.{s}"), params));
        }
    }
    // Externs that link to the same symbol share its declaration, so
    // they must agree on its signature. One that none of them says may
    // throw doesn't unwind: if it did, that would be undefined behavior.
    let mut throwing: BTreeSet<&str> = BTreeSet::new();
    for ext in m.externs.values() {
        if let Link::C { symbol, .. } = &ext.link
            && ext.sig.row.throws()
        {
            throwing.insert(symbol);
        }
    }
    let mut declared: BTreeMap<&str, (ExternId, (Type, Vec<Type>))> = BTreeMap::new();
    for (&id, ext) in &m.externs {
        let Link::C { symbol, .. } = &ext.link else {
            continue;
        };
        let sig =
            extern_sig(ext).map_err(|message| err(None, format!("extern {id}: {message}")))?;
        match declared.get(&**symbol) {
            Some((first, other)) if *other != sig => {
                return Err(err(
                    None,
                    format!(
                        "externs {first} and {id} both link to `{symbol}`, with different signatures"
                    ),
                ));
            }
            Some(_) => {}
            None => {
                let attrs = if throwing.contains(&**symbol) {
                    vec![]
                } else {
                    vec!["nounwind"]
                };
                out.push(Declare {
                    attrs,
                    ..Declare::new(sig.0.clone(), symbol.clone(), sig.1.clone())
                });
                declared.insert(symbol, (id, sig));
            }
        }
    }
    let mut entries = BTreeSet::new();
    for (&c, decl) in &m.consts {
        if !matches!(decl.value, ConstValue::Lit(_)) {
            const_global(
                m,
                &mut out,
                &mut entries,
                &const_name(c),
                &decl.value,
                &decl.ty,
            )
            .map_err(|message| err(None, format!("constant {c}: {message}")))?;
        }
    }
    let callbacks = callback_sigs(m);
    if !callbacks.is_empty() {
        out.push(Declare::new(
            host_pair(),
            "conduit_host_function",
            vec![Type::Ptr, Type::Ptr],
        ));
    }
    out.items
        .extend((hs.items(m, options.counts)).map_err(|message| err(None, message))?);
    let mut kinds = BTreeSet::new();
    for (&id, f) in &m.funcs {
        let lowered =
            Lower::new(m, id, f, options, &callbacks, &hs, debug.as_ref()).and_then(Lower::func);
        let (func, fn_entries, fn_kinds) = lowered.map_err(|message| err(Some(id), message))?;
        out.push(func);
        entries.extend(fn_entries);
        kinds.extend(fn_kinds);
    }
    for &(f, n) in &entries {
        out.push(entry(m, f, n, options.counts).map_err(|message| err(Some(f), message))?);
        if options.counts {
            let caps = &m.funcs[&f].sig().params[..n];
            let (offsets, _) = closure_layout(caps);
            kinds.extend(caps.iter().filter_map(Kind::of));
            out.push(counts::closure_free(free_name(f, n), caps, &offsets));
            let visit = if options.cycles {
                out.push(counts::closure_visit(visit_name(f, n), caps, &offsets));
                Value::Global(visit_name(f, n))
            } else {
                Value::Null
            };
            let free = Value::Global(free_name(f, n));
            out.push(counts::descriptor(desc_name(f, n), free, visit));
        }
    }
    // What the host calls the closures of `host_function` through, and
    // lets go of them with.
    for (k, (params, results)) in callbacks.iter().enumerate() {
        let tramp = trampoline(k, params, results, options.counts, Handlers::of(m));
        out.push(tramp.map_err(|message| err(None, message))?);
    }
    if !callbacks.is_empty() {
        out.push(release_fn(options.counts));
    }
    if options.counts {
        // Entry functions drop their closure, and so does
        // `conduit_release`.
        if !entries.is_empty() || !callbacks.is_empty() {
            kinds.insert(Kind::Closure);
        }
        // An export or trampoline that gives a host value drops its box.
        let gives_host = (m.exports.iter())
            .filter(|(_, e)| matches!(e, conduit_ir::Export::C { .. }))
            .any(|(id, _)| m.funcs[id].results.iter().any(Ty::is_host))
            || callbacks.iter().any(|(_, r)| r.iter().any(Ty::is_host));
        if gives_host {
            kinds.insert(Kind::Host);
        }
        out.items.extend(counts::runtime(options.cycles));
        let drops = counts::drops(m, &kinds, options.cycles);
        out.items
            .extend(drops.map_err(|message| err(None, message))?);
    }
    for (&id, export) in &m.exports {
        let conduit_ir::Export::C { symbol } = export else {
            continue;
        };
        let f = &m.funcs[&id];
        let wrapper = export_fn(f, id, symbol, options.counts, Handlers::of(m));
        out.push(wrapper.map_err(|message| err(Some(id), message))?);
    }
    if let Some(debug) = debug {
        out.metadata = debug.into_inner().finish();
    }
    Ok(out)
}

/// The sources of the `let`s and `jump`s of `body`, in the order the
/// blocks are lowered in.
fn sources_of(body: &Block) -> impl Iterator<Item = SourceId> + '_ {
    body.blocks().flat_map(|b| {
        let jump = match &b.tail {
            Tail::Jump { source, .. } => *source,
            _ => None,
        };
        b.lets.iter().filter_map(|l| l.source).chain(jump)
    })
}

/// The C function `symbol` that calls function `id`, `f`, as an export.
fn export_fn(
    f: &Func,
    id: FuncId,
    symbol: &str,
    counts: bool,
    handlers: Handlers,
) -> Result<ir::Function, String> {
    let call = |_: &mut FnBuilder, ret: Type, args| Call::tailcc(ret, self::symbol(id), args);
    c_entry(
        symbol,
        0,
        &f.sig().params,
        &f.results,
        counts,
        handlers,
        call,
    )
}

/// What `host_function` gives the host for a closure that takes `params`
/// and gives `results`, the `k`th such signature: a C function
/// `conduit_trampoline<k>(void *closure, params..)`, which runs the
/// closure as an export runs its function. The host keeps its reference
/// to the closure, and applying it takes one, so it adds one first.
fn trampoline(
    k: usize,
    params: &[Ty],
    results: &[Ty],
    counts: bool,
    handlers: Handlers,
) -> Result<ir::Function, String> {
    let call = |b: &mut FnBuilder, ret: Type, args: Vec<Typed>| {
        let c = args[0].1.clone();
        if counts {
            call_void(b, Value::global("conduit_dup"), vec![Type::Ptr.val(&c)]);
        }
        let entry = b.load_at(Type::Ptr, &c, 8);
        Call::tailcc(ret, entry, args)
    };
    c_entry(
        &trampoline_name(k),
        1,
        params,
        results,
        counts,
        handlers,
        call,
    )
}

/// The `k`th trampoline of `host_function`.
fn trampoline_name(k: usize) -> String {
    format!("conduit_trampoline{k}")
}

/// `conduit_release(closure)`, which the host calls when it lets go of a
/// function `host_function` gave it: it drops the closure.
fn release_fn(counts: bool) -> ir::Function {
    let params = vec![(Type::Ptr, "c".to_string())];
    let mut b = FnBuilder::new(CallConv::C, Type::Void, "conduit_release", params);
    if counts {
        let c = b.param(0);
        call_void(&mut b, Kind::Closure.drop_fn(), vec![Type::Ptr.val(c)]);
    }
    b.ret_void();
    b.finish()
}

/// A C function `name` that the host calls, an export's or a trampoline:
/// it takes `lead` pointers as they are, then `params` as C passes them,
/// and a pointer for each result after the first, and gives `results` as
/// C takes them (see [`c_results`]). A host value comes in as a
/// [`host_pair`], whose reference the caller gives Conduit, and goes in a
/// box; one going out is retained for the caller, and its box dropped.
/// `call` makes the call, of what returns `Type`, with the arguments as
/// Conduit has them. With `handlers`, it saves the handler stack, runs
/// with none, and puts it back, also when an exception unwinds it, if the
/// module [throws](Handlers::throws).
fn c_entry(
    name: &str,
    lead: usize,
    tys: &[Ty],
    results: &[Ty],
    counts: bool,
    handlers: Handlers,
    call: impl FnOnce(&mut FnBuilder, Type, Vec<Typed>) -> Call,
) -> Result<ir::Function, String> {
    let mut params: Vec<(Type, String)> = (0..lead).map(|i| (Type::Ptr, format!("c{i}"))).collect();
    for (i, t) in tys.iter().enumerate() {
        if t.is_host() {
            params.push((Type::Ptr, format!("p{i}.value")));
            params.push((Type::Ptr, format!("p{i}.ops")));
        } else {
            params.push((ty(t)?, format!("p{i}")));
        }
    }
    // Where the caller wants each result after the first.
    let (ret, out_tys) = c_results(results)?;
    let first_out = params.len();
    for i in 1..results.len() {
        params.push((Type::Ptr, format!("r{i}")));
    }
    let inner = ret_ty(results)?;
    let mut b = FnBuilder::new(CallConv::C, ret.clone(), name.to_string(), params);
    let mut args: Vec<Typed> = (0..lead).map(|i| Type::Ptr.val(b.param(i))).collect();
    let mut k = lead;
    for t in tys {
        if t.is_host() {
            let (value, ops) = (b.param(k), b.param(k + 1));
            k += 2;
            args.push(Type::Ptr.val(host_in(&mut b, counts, value, ops, *t == Ty::NullableHost)));
        } else {
            args.push(ty(t)?.val(b.param(k)));
            k += 1;
        }
    }
    // It runs with no handlers, the host's caller's being out of reach,
    // and puts theirs back after.
    let stack = handlers::handlers_global();
    let saved = handlers.any.then(|| {
        let saved = b.load(Type::Ptr, &stack);
        b.store(Type::Ptr, Value::Null, &stack);
        saved
    });
    let call = call(&mut b, inner.clone(), args);
    if saved.is_none() && results.len() <= 1 && !results.iter().any(Ty::is_host) {
        b.return_call(call);
        return Ok(b.finish());
    }
    // In a module that throws, the host may catch an exception that
    // unwinds this, and go on: it gets its handler stack back then too.
    let pad = saved
        .as_ref()
        .filter(|_| handlers.throws)
        .map(|saved| (b.new_block(), saved.clone()));
    let r = match &pad {
        Some((pad, _)) => {
            b.personality(Value::global(handlers::PERSONALITY));
            b.invoke(call, pad)
        }
        None => b.call(call),
    };
    if let Some(saved) = &saved {
        b.store(Type::Ptr, saved.clone(), &stack);
    }
    let mut gave = Vec::with_capacity(results.len());
    for (i, t) in results.iter().enumerate() {
        let r = r.clone().expect("a call with results");
        let v = if results.len() == 1 {
            r
        } else {
            b.extract_value(inner.clone(), r, i as u32)
        };
        gave.push(if t.is_host() {
            host_give(&mut b, counts, v, *t == Ty::NullableHost)
        } else {
            v
        });
    }
    let mut gave = gave.into_iter();
    let first = gave.next();
    for (k, (v, t)) in gave.zip(out_tys).enumerate() {
        let p = b.param(first_out + k);
        b.store(t, v, p);
    }
    match first {
        Some(v) => b.ret(Some(ret.val(v))),
        None => b.ret_void(),
    }
    if let Some((pad, saved)) = pad {
        b.start(&pad);
        let landed = b.landing_pad();
        b.store(Type::Ptr, saved, &stack);
        b.resume(landed);
    }
    Ok(b.finish())
}

/// What an entry point of the host's must do about handlers.
#[derive(Clone, Copy)]
struct Handlers {
    /// Whether the module has any: then it runs with none, and puts the
    /// caller's back.
    any: bool,
    /// Whether an exception from the host can unwind the module's code:
    /// some function has `throw` in its row. Otherwise one that unwinds an
    /// entry point is undefined behavior, so it needn't put things back
    /// then.
    throws: bool,
}

impl Handlers {
    fn of(m: &Module) -> Handlers {
        Handlers {
            any: !m.effects.is_empty(),
            throws: m.funcs.values().any(|f| f.row.throws()),
        }
    }
}

/// The signatures of the closures that `host_function`s in `m` take, as
/// their parameters and results, each once, in the order they're met.
fn callback_sigs(m: &Module) -> Vec<(Vec<Ty>, Vec<Ty>)> {
    let mut sigs = Vec::new();
    for f in m.funcs.values() {
        for l in f.body.all_lets() {
            if l.op == Op::HostFunction
                && let Some(Atom::Var(c)) = l.args.first()
                && let Ty::Fn(sig, _) = f.var_ty(*c)
            {
                let key = (sig.params.clone(), sig.results.clone());
                if !sigs.contains(&key) {
                    sigs.push(key);
                }
            }
        }
    }
    sigs
}

/// What box `p` holds as a [`host_pair`] for the host: its value and ops,
/// retained, as the host now holds a reference too, and then the box's
/// reference dropped. A null box is a null value.
fn host_give(b: &mut FnBuilder, counts: bool, p: Value, nullable: bool) -> Value {
    let (vslot, oslot) = (b.alloca(Type::Ptr), b.alloca(Type::Ptr));
    b.store(Type::Ptr, Value::Null, &vslot);
    b.store(Type::Ptr, Value::Null, &oslot);
    let (some, done) = (b.new_block(), b.new_block());
    if nullable {
        let has = b.icmp(IntPred::Ne, Type::Ptr, &p, Value::Null);
        b.cond_br(has, &some, &done);
    } else {
        b.br(&some);
    }
    b.start(&some);
    let value = b.load_at(Type::Ptr, &p, 8);
    let ops = b.load_at(Type::Ptr, &p, 16);
    let retain = b.load(Type::Ptr, &ops);
    b.call(Call::c(Type::Void, retain, vec![Type::Ptr.val(&value)]));
    b.store(Type::Ptr, value, &vslot);
    b.store(Type::Ptr, ops, &oslot);
    if counts {
        b.call(Call::tailcc(
            Type::Void,
            Kind::Host.drop_fn(),
            vec![Type::Ptr.val(&p)],
        ));
    }
    b.br(&done);
    b.start(&done);
    let value = b.load(Type::Ptr, &vslot);
    let ops = b.load(Type::Ptr, &oslot);
    let pair = b.insert_value(host_pair(), Value::Undef, Type::Ptr.val(value), 0);
    b.insert_value(host_pair(), pair, Type::Ptr.val(ops), 1)
}

/// A global for constant `value` of type `t`, as an object laid out as
/// at run time, immortal: its header has the sign bit set, which counting
/// will skip. An object inside it is a global of its own. A closure's
/// entry function goes in `entries`.
fn const_global(
    m: &Module,
    out: &mut ir::Module,
    entries: &mut BTreeSet<(FuncId, usize)>,
    name: &str,
    value: &ConstValue,
    t: &Ty,
) -> Result<(), String> {
    // A closure's and an `any`'s pointers at 16 are never called, as
    // nothing frees an immortal object: they're null.
    let (word, tys, values, after): (u64, Vec<Ty>, &[ConstValue], Vec<Typed>) = match (value, t) {
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
            let entry = Type::Ptr.val(Value::Global(entry_name(*f, 0)));
            (0, Vec::new(), &[], vec![entry, Type::Ptr.val(Value::Null)])
        }
        (ConstValue::Box(bt, inner), _) => (
            0,
            vec![bt.clone()],
            std::slice::from_ref(&**inner),
            vec![Type::Ptr.val(Value::Null)],
        ),
        _ => return Err(format!("a constant of type {t} isn't supported yet")),
    };
    let padding = |n: u64| Typed(Type::Array(n, Box::new(Type::I8)), Value::Zero);
    let (offsets, total) = layout(&tys);
    let mut parts = vec![Type::I32.val(i32::MIN), Type::I32.val(word)];
    let mut at: u64 = 8;
    for (k, ((v, ft), &o)) in values.iter().zip(&tys).zip(&offsets).enumerate() {
        if o > at {
            parts.push(padding(o - at));
        }
        let value = match v {
            ConstValue::Lit(l) => lit(l),
            ConstValue::Const(c) => match &m.consts.get(c).ok_or("an unknown constant")?.value {
                ConstValue::Lit(l) => lit(l),
                _ => Value::Global(const_name(*c)),
            },
            inner => {
                let sub = format!("{name}_{k}");
                const_global(m, out, entries, &sub, inner, ft)?;
                Value::Global(sub)
            }
        };
        parts.push(ty(ft)?.val(value));
        at = o + size(ft);
    }
    // After the fields, up to 16 for an `any`'s.
    let total = if after.is_empty() {
        total
    } else {
        total.max(at.next_multiple_of(8))
    };
    if total > at {
        parts.push(padding(total - at));
    }
    parts.extend(after);
    let tys = parts.iter().map(|p| p.0.clone()).collect();
    out.push(Global {
        align: Some(8),
        ..Global::constant(name, Type::Packed(tys), Value::Packed(parts))
    });
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
fn entry(m: &Module, f: FuncId, n: usize, count: bool) -> Result<ir::Function, String> {
    let func = &m.funcs[&f];
    let sig = func.sig();
    let ret = ret_ty(&sig.results)?;
    let (offsets, _) = closure_layout(&sig.params[..n]);
    let mut params = vec![(Type::Ptr, "c".to_string())];
    for (i, t) in sig.params[n..].iter().enumerate() {
        params.push((ty(t)?, format!("p{i}")));
    }
    let mut b = FnBuilder::new(
        CallConv::Tail,
        ret.clone(),
        entry_name(f, n),
        params.clone(),
    );
    let c = b.param(0);
    let mut args = Vec::new();
    for (t, &o) in sig.params[..n].iter().zip(&offsets) {
        let lt = ty(t)?;
        let k = b.load_at(lt.clone(), &c, o);
        // `apply` takes the closure, and the function owns its captures:
        // each gets a count, and the closure is dropped.
        if count && Kind::of(t).is_some() {
            call_void(
                &mut b,
                Value::global("conduit_dup"),
                vec![Type::Ptr.val(&k)],
            );
        }
        args.push(lt.val(k));
    }
    if count {
        call_void(&mut b, Kind::Closure.drop_fn(), vec![Type::Ptr.val(&c)]);
    }
    for (i, (t, _)) in params.iter().enumerate().skip(1) {
        args.push(t.val(b.param(i)));
    }
    b.return_call(Call::tailcc(ret, symbol(f), args).tail());
    Ok(b.finish())
}

/// What lowering a function gives: the function, the closure entry
/// functions it needs, and the kinds of what it drops.
type Lowered = (ir::Function, BTreeSet<(FuncId, usize)>, BTreeSet<Kind>);

/// The lowering of one function.
struct Lower<'m> {
    m: &'m Module,
    f: &'m Func,
    defs: Vec<Option<&'m JoinDef>>,
    ret: Type,
    b: FnBuilder,
    /// Each variable's slot.
    vars: Vec<Value>,
    /// The closure entry functions the function needs.
    entries: BTreeSet<(FuncId, usize)>,
    /// The signatures of closures given to `host_function`, by the number
    /// of their trampoline: see [`callback_sigs`].
    callbacks: &'m [(Vec<Ty>, Vec<Ty>)],
    handlers: &'m handlers::Handlers,
    /// Whether to count, and the kinds of what it drops.
    counts: bool,
    cycles: bool,
    kinds: BTreeSet<Kind>,
    /// When counting, what the frame owns at each `let`, to drop when an
    /// abort unwinds it, if one can.
    owned: Option<conduit_ir::owned::Owned<'m>>,
    /// While lowering a `let` that an abort can unwind while the frame
    /// owns something: the landing pad its calls unwind to, and what it
    /// drops.
    unwind: Option<(ir::Label, Vec<(Var, u32)>)>,
    /// Those landing pads, which go after the body.
    pads: Vec<(ir::Label, Vec<(Var, u32)>)>,
    /// The module's debug info, and the function's `DISubprogram`, if it
    /// has one.
    debug: Option<&'m RefCell<debug::Debug>>,
    subprogram: Option<u32>,
}

impl<'m> Lower<'m> {
    fn new(
        m: &'m Module,
        id: FuncId,
        f: &'m Func,
        options: &Options,
        callbacks: &'m [(Vec<Ty>, Vec<Ty>)],
        handlers: &'m handlers::Handlers,
        debug: Option<&'m RefCell<debug::Debug>>,
    ) -> Result<Lower<'m>, String> {
        let params = (f.params.iter())
            .map(|v| Ok((ty(f.var_ty(*v))?, format!("a{}", v.0))))
            .collect::<Result<_, String>>()?;
        let ret = ret_ty(&f.results)?;
        let mut b = FnBuilder::new(CallConv::Tail, ret.clone(), fn_name(id), params);
        // A `DISubprogram` at its first source the frontend placed, and
        // everything at that until a `let` or `jump` says otherwise.
        let first = debug.and_then(|d| {
            let d = d.borrow();
            sources_of(&f.body).find_map(|s| d.location_of(s))
        });
        let subprogram = match (debug, first) {
            (Some(d), Some(at)) => {
                let mut d = d.borrow_mut();
                let name = f.debug.name.as_deref().map_or(fn_name(id), str::to_string);
                let sp = d.subprogram(&name, &fn_name(id), &at);
                b.subprogram(sp);
                b.set_loc(Some(d.location(sp, &at)));
                Some(sp)
            }
            _ => None,
        };
        // A slot for each variable, and the parameters stored in theirs.
        let vars: Vec<Value> = (f.vars.iter().enumerate())
            .map(|(i, t)| {
                let t = ty(t).map_err(|e| format!("%{i}: {e}"))?;
                Ok(b.named_alloca(t, format!("v{i}")))
            })
            .collect::<Result<_, String>>()?;
        for (i, v) in f.params.iter().enumerate() {
            let p = b.param(i);
            b.store(ty(f.var_ty(*v))?, p, &vars[v.0 as usize]);
        }
        Ok(Lower {
            m,
            f,
            defs: conduit_ir::tail::join_defs(&f.body, f.labels),
            ret,
            b,
            vars,
            entries: BTreeSet::new(),
            callbacks,
            handlers,
            counts: options.counts,
            cycles: options.counts && options.cycles,
            kinds: BTreeSet::new(),
            owned: (options.counts && conduit_ir::owned::unwinds(m, &f.row)).then(|| {
                conduit_ir::owned::Owned::new(f, |g, i| {
                    m.funcs.get(&g).is_some_and(|g| g.is_borrowed(i))
                })
            }),
            unwind: None,
            pads: Vec::new(),
            debug,
            subprogram,
        })
    }

    /// Lowers what comes next at `source`, if it has one and the function
    /// has debug info.
    fn at(&mut self, source: Option<SourceId>) {
        let (Some(d), Some(sp), Some(source)) = (self.debug, self.subprogram, source) else {
            return;
        };
        let at = d.borrow().location_of(source);
        if let Some(at) = at {
            let loc = d.borrow_mut().location(sp, &at);
            self.b.set_loc(Some(loc));
        }
    }

    fn func(mut self) -> Result<Lowered, String> {
        self.block(&self.f.body)?;
        // An abort that unwinds the frame drops what it owns, and goes on.
        for (pad, owned) in std::mem::take(&mut self.pads) {
            self.b.start(&pad);
            let landed = self.b.landing_pad();
            self.drop_owned(&owned)?;
            self.b.resume(landed);
        }
        Ok((self.b.finish(), self.entries, self.kinds))
    }

    /// Drops what the frame owns, `owned`, as an abort unwinds it.
    fn drop_owned(&mut self, owned: &[(Var, u32)]) -> Result<(), String> {
        for &(v, n) in owned {
            let t = self.f.var_ty(v);
            let Some(k) = Kind::of(t) else { continue };
            let x = self.value(&Atom::Var(v))?;
            let drop = self.drop_of(t, k);
            for _ in 0..n {
                call_void(&mut self.b, drop.clone(), vec![Type::Ptr.val(&x)]);
            }
        }
        Ok(())
    }

    /// A call that an abort or an exception from the host can unwind: an
    /// `invoke` of the landing pad of the `let` being lowered, if it has
    /// one.
    fn call_out(&mut self, call: Call) -> Option<Value> {
        match self.unwind.as_ref().map(|(pad, _)| pad.clone()) {
            Some(pad) => {
                self.b.personality(Value::global(handlers::PERSONALITY));
                self.b.invoke(call, &pad)
            }
            None => self.b.call(call),
        }
    }

    fn atom_ty(&self, a: &Atom) -> Ty {
        self.f.atom_ty(a)
    }

    /// An atom's value, loaded if it's a variable.
    fn value(&mut self, a: &Atom) -> Result<Value, String> {
        match a {
            Atom::Lit(l) => Ok(lit(l)),
            Atom::Var(v) => {
                let t = ty(self.f.var_ty(*v))?;
                Ok(self.b.load(t, &self.vars[v.0 as usize]))
            }
        }
    }

    fn store(&mut self, v: Var, value: impl Into<Value>) -> Result<(), String> {
        let t = ty(self.f.var_ty(v))?;
        self.b.store(t, value, &self.vars[v.0 as usize]);
        Ok(())
    }

    fn bind(&mut self, binds: &[Var], value: impl Into<Value>) -> Result<(), String> {
        match binds.first() {
            Some(&b) => self.store(b, value),
            None => Ok(()),
        }
    }

    /// A new object of `size` bytes, an `i64` constant or value, with its
    /// count at 1 and `word` at 4. Trapping if there's no memory left.
    fn alloc(&mut self, size: impl Into<Value>, word: impl Into<Value>) -> Value {
        alloc(&mut self.b, self.counts, size, word)
    }

    fn block(&mut self, b: &Block) -> Result<(), String> {
        conduit_ir::deep(|| {
            let owned = self.owned.as_ref().map(|o| o.during_each(b));
            for (i, l) in b.lets.iter().enumerate() {
                self.at(l.source);
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
                    self.tail_call(symbol(g), args);
                    return Ok(());
                }
                if tail
                    && l.op == Op::Apply
                    && let Ty::Fn(sig, _) = self.atom_ty(&l.args[0])
                    && sig.results == self.f.results
                {
                    let (fp, args) = self.apply_args(&l.args)?;
                    self.tail_call(fp, args);
                    return Ok(());
                }
                let owned = owned.as_ref().map_or(&[][..], |o| &o[i][..]);
                if !owned.is_empty() && conduit_ir::owned::may_unwind(self.m, self.f, l) {
                    let pad = self.b.new_block();
                    self.pads.push((pad.clone(), owned.to_vec()));
                    self.unwind = Some((pad, owned.to_vec()));
                }
                let r = self.let_(&l.op, &l.args, &l.binds);
                self.unwind = None;
                r?;
            }
            self.tail(&b.tail)
        })
    }

    /// A call that returns what it gives, which `tailcc` makes a tail call.
    fn tail_call(&mut self, callee: Value, args: Vec<Typed>) {
        let call = Call::tailcc(self.ret.clone(), callee, args).tail();
        self.b.return_call(call);
    }

    fn args(&mut self, args: &[Atom]) -> Result<Vec<Typed>, String> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            let t = ty(&self.atom_ty(a))?;
            let v = self.value(a)?;
            out.push(Typed(t, v));
        }
        Ok(out)
    }

    /// The entry function to call for `apply(c, args..)`, and the
    /// arguments: the closure, then `args`.
    fn apply_args(&mut self, args: &[Atom]) -> Result<(Value, Vec<Typed>), String> {
        let c = self.value(&args[0])?;
        let fp = self.b.load_at(Type::Ptr, &c, 8);
        let mut all = vec![Type::Ptr.val(c)];
        all.extend(self.args(&args[1..])?);
        Ok((fp, all))
    }

    /// Makes `call`, and puts its results into `binds`.
    fn bind_results(&mut self, call: Call, binds: &[Var]) -> Result<(), String> {
        let ret = call.ret.clone();
        let Some(r) = self.call_out(call) else {
            return Ok(());
        };
        if let Type::Struct(_) = ret {
            for (i, &b) in binds.iter().enumerate() {
                let x = self.b.extract_value(ret.clone(), &r, i as u32);
                self.store(b, x)?;
            }
            Ok(())
        } else {
            self.bind(binds, r)
        }
    }

    /// `handle h(env.., body)`: pushes a frame of `h`, applies the body,
    /// pops the frame and calls the return clause with the environment
    /// and the body's results. If `h` has an aborting clause, the body is
    /// `invoke`d, and a clause that aborts to the frame unwinds to its
    /// landing pad, which gives the frame's results. See `handlers`.
    fn handle(
        &mut self,
        h: conduit_ir::HandlerId,
        args: &[Atom],
        binds: &[Var],
    ) -> Result<(), String> {
        let m = self.m;
        let decl = &m.handlers[&h];
        let frame = self.handlers.frame(m, h)?;
        let (env, body) = args.split_at(args.len() - 1);
        let stack = handlers::handlers_global();
        // 16-aligned, for the exception in it.
        let at = self
            .b
            .aligned_alloca(Type::Array(frame.words, Box::new(Type::I64)), 16);
        let saved = self.b.load(Type::Ptr, &stack);
        self.b.store(Type::I32, Value::Int(h.0 as i64), &at);
        self.b.store_at(Type::Ptr, &saved, &at, 8);
        for (a, &o) in env.iter().zip(&frame.env) {
            let t = ty(&self.atom_ty(a))?;
            let v = self.value(a)?;
            self.b.store_at(t, v, &at, o);
        }
        self.b.store(Type::Ptr, &at, &stack);
        let aborted = frame.exception.map(|exn| {
            self.b.personality(Value::global(handlers::PERSONALITY));
            // A clause that discontinued may have changed the site.
            let site = (self.handlers.discontinues())
                .then(|| self.b.load(Type::Ptr, handlers::site_global()));
            (self.b.new_block(), exn, site)
        });
        let Ty::Fn(sig, _) = self.atom_ty(&body[0]) else {
            return Err(format!("the body of handle {h} isn't a closure"));
        };
        let gives = ret_ty(&sig.results)?;
        let (fp, all) = self.apply_args(body)?;
        let call = Call::tailcc(gives.clone(), fp, all);
        // When counting, an abort that unwinds the `handle` drops the
        // environment, which the return clause won't take.
        let env_counted = self.counts && decl.env.iter().any(|t| Kind::of(t).is_some());
        let cleanup = (aborted.is_none() && env_counted).then(|| self.b.new_block());
        let r = match (&aborted, &cleanup) {
            (Some((pad, ..)), _) | (None, Some(pad)) => {
                self.b.personality(Value::global(handlers::PERSONALITY));
                self.b.invoke(call, pad)
            }
            (None, None) => self.call_out(call),
        };
        self.b.store(Type::Ptr, &saved, &stack);
        let ret_f =
            (m.funcs.get(&decl.ret)).ok_or_else(|| format!("unknown clause {}", decl.ret))?;
        let mut rargs = self.args(env)?;
        for (j, t) in sig.results.iter().enumerate() {
            let r = r.clone().expect("a call with results");
            let v = if sig.results.len() == 1 {
                r
            } else {
                self.b.extract_value(gives.clone(), r, j as u32)
            };
            rargs.push(ty(t)?.val(v));
        }
        let ret = ret_ty(&ret_f.results)?;
        self.bind_results(Call::tailcc(ret, symbol(decl.ret), rargs), binds)?;
        if let Some(pad) = cleanup {
            let done = self.b.new_block();
            self.b.br(&done);
            self.b.start(&pad);
            let landed = self.b.landing_pad();
            self.unwinding(h, &at, &frame)?;
            self.b.resume(landed);
            self.b.start(&done);
        }
        let Some((pad, exn, site)) = aborted else {
            return Ok(());
        };
        let (done, mine, other) = (self.b.new_block(), self.b.new_block(), self.b.new_block());
        self.b.br(&done);
        // A clause aborted, to this frame if the exception is its own.
        self.b.start(&pad);
        let landed = self.b.landing_pad();
        let thrown = self.b.extract_value(ir::landing_pad_ty(), &landed, 0);
        let own = self.b.offset(&at, exn);
        let is_mine = self.b.icmp(IntPred::Eq, Type::Ptr, thrown, own);
        self.b.cond_br(is_mine, &mine, &other);
        self.b.start(&other);
        self.unwinding(h, &at, &frame)?;
        self.b.resume(landed);
        // It left the results in the frame, and the return clause won't
        // take the environment.
        self.b.start(&mine);
        self.drop_env(h, &at, &frame)?;
        self.b.store(Type::Ptr, &saved, &stack);
        if let Some(site) = site {
            self.b.store(Type::Ptr, site, handlers::site_global());
        }
        for (&v, &o) in binds.iter().zip(&frame.results) {
            let t = ty(self.f.var_ty(v))?;
            let x = self.b.load_at(t, &at, o);
            self.store(v, x)?;
        }
        self.b.br(&done);
        self.b.start(&done);
        Ok(())
    }

    /// When counting, drops the environment of `h` in its frame at `at`,
    /// laid out as `frame`.
    fn drop_env(
        &mut self,
        h: conduit_ir::HandlerId,
        at: &Value,
        frame: &handlers::Frame,
    ) -> Result<(), String> {
        if !self.counts {
            return Ok(());
        }
        for (t, &off) in self.m.handlers[&h].env.iter().zip(&frame.env) {
            if let Some(k) = Kind::of(t) {
                let x = self.b.load_at(Type::Ptr, at, off);
                let drop = self.drop_of(t, k);
                call_void(&mut self.b, drop, vec![Type::Ptr.val(x)]);
            }
        }
        Ok(())
    }

    /// An abort unwinds a `handle` of `h`, with its frame at `at`: it
    /// drops the environment, and what the frame owns while the `handle`
    /// runs.
    fn unwinding(
        &mut self,
        h: conduit_ir::HandlerId,
        at: &Value,
        frame: &handlers::Frame,
    ) -> Result<(), String> {
        self.drop_env(h, at, frame)?;
        if let Some((_, owned)) = self.unwind.clone() {
            self.drop_owned(&owned)?;
        }
        Ok(())
    }

    /// `perform e.i(args)`: finds the nearest frame of a handler with a
    /// clause for it, trapping if there's none, and calls the clause's
    /// wrapper with the frames below it as the handlers, then puts them
    /// back. See `handlers`.
    fn perform(
        &mut self,
        e: conduit_ir::EffectId,
        i: u32,
        args: &[Atom],
        binds: &[Var],
    ) -> Result<(), String> {
        let m = self.m;
        let op = &m.effects[&e].ops[i as usize];
        let ret = ret_ty(&op.results)?;
        let args = self.args(args)?;
        let stack = handlers::handlers_global();
        let saved = self.b.load(Type::Ptr, &stack);
        let at = self.b.alloca(Type::Ptr);
        self.b.store(Type::Ptr, &saved, &at);
        let (look, next, done) = (self.b.new_block(), self.b.new_block(), self.b.new_block());
        self.b.br(&look);
        self.b.start(&look);
        let found = self.b.load(Type::Ptr, &at);
        // No frame left: nothing handles it.
        let none = self.b.icmp(IntPred::Eq, Type::Ptr, &found, Value::Null);
        self.trap_if(none);
        let number = self.b.load(Type::I32, &found);
        let hs = handlers::handlers_of(m, e, i);
        let blocks: Vec<ir::Label> = hs.iter().map(|_| self.b.new_block()).collect();
        let cases = (hs.iter().zip(&blocks))
            .map(|((h, _), l)| (h.0 as i64, l.clone()))
            .collect();
        self.b.switch(Type::I32.val(number), &next, cases);
        self.b.start(&next);
        let below = self.b.load_at(Type::Ptr, &found, 8);
        self.b.store(Type::Ptr, below, &at);
        self.b.br(&look);
        for (&(h, slot), block) in hs.iter().zip(&blocks) {
            self.b.start(block);
            let below = self.b.load_at(Type::Ptr, &found, 8);
            self.b.store(Type::Ptr, below, &stack);
            // The clause can discontinue under this `perform`'s handlers.
            let site = (self.handlers.discontinues()).then(|| {
                let old = self.b.load(Type::Ptr, handlers::site_global());
                self.b.store(Type::Ptr, &saved, handlers::site_global());
                old
            });
            let mut all = vec![Type::Ptr.val(&found)];
            all.extend(args.iter().cloned());
            let wrapper = Value::global(handlers::wrapper_name(h, slot));
            self.bind_results(Call::tailcc(ret.clone(), wrapper, all), binds)?;
            self.b.store(Type::Ptr, &saved, &stack);
            if let Some(old) = site {
                self.b.store(Type::Ptr, old, handlers::site_global());
            }
            self.b.br(&done);
        }
        self.b.start(&done);
        Ok(())
    }

    /// The type of the array `a`'s elements.
    fn elem(&self, a: &Ty) -> Result<Ty, String> {
        match a {
            Ty::Array(e, _) => Ok((**e).clone()),
            other => Err(format!("an array op on a {other}")),
        }
    }

    /// Traps if `i` isn't an index of array `a`.
    fn check_index(&mut self, a: &Value, i: &Value) {
        let len = self.b.load_at(Type::I32, a, 4);
        let out = self.b.icmp(IntPred::Uge, Type::I32, i, len);
        self.trap_if(out);
    }

    /// Traps unless the `n` bytes of array `a` from `at`, an `i32` value,
    /// are all in it. In 64 bits, so that the end can't wrap around.
    fn check_bytes(&mut self, a: &Value, at: &Value, n: u64) {
        let len = self.b.load_at(Type::I32, a, 4);
        let len = self.b.cast(CastOp::ZExt, Type::I32, len, Type::I64);
        let start = self.b.cast(CastOp::ZExt, Type::I32, at, Type::I64);
        let end = self.b.bin(BinOp::Add, Type::I64, start, n);
        let out = self.b.icmp(IntPred::Ugt, Type::I64, &end, len);
        self.trap_if(out);
    }

    /// A new array of `n` elements, an `i32` value, trapping if `n` is
    /// negative.
    fn new_array(&mut self, n: &Value, stride: u64) -> Value {
        let neg = self.b.icmp(IntPred::Slt, Type::I32, n, 0);
        self.trap_if(neg);
        let bytes = elem_offset(&mut self.b, n, stride);
        self.alloc(bytes, n)
    }

    fn let_(&mut self, op: &Op, args: &[Atom], binds: &[Var]) -> Result<(), String> {
        match op {
            Op::Prim(p) => {
                let r = self.prim(p, args)?;
                self.bind(binds, r)?;
            }
            Op::Call(g, ..) => {
                let callee = self
                    .m
                    .funcs
                    .get(g)
                    .ok_or_else(|| format!("unknown function {g}"))?;
                let ret = ret_ty(&callee.results)?;
                let args = self.args(args)?;
                self.bind_results(Call::tailcc(ret, symbol(*g), args), binds)?;
            }
            Op::CallExtern(e) => self.call_extern(*e, args, binds)?,
            // C's personality can't catch (docs/ir/exceptions.md).
            Op::CatchExtern(e) => {
                return Err(format!(
                    "catch_extern of {e}: native code can't catch what the host throws yet (#66)"
                ));
            }
            Op::Handle(h, _) => self.handle(*h, args, binds)?,
            Op::Perform(e, i) => self.perform(*e, *i, args, binds)?,
            // In a tail-resumptive clause, without its continuation: the
            // `perform` it handles performs this instead, under its
            // handlers.
            Op::Discontinue(e, i) => {
                let site = self.b.load(Type::Ptr, handlers::site_global());
                self.b.store(Type::Ptr, site, handlers::handlers_global());
                self.perform(*e, *i, args, binds)?;
            }
            Op::Closure(f, ..) => {
                let tys: Vec<Ty> = args.iter().map(|a| self.atom_ty(a)).collect();
                let (offsets, total) = closure_layout(&tys);
                self.entries.insert((*f, args.len()));
                let p = self.alloc(total, 0);
                let entry = Value::Global(entry_name(*f, args.len()));
                self.b.store_at(Type::Ptr, entry, &p, 8);
                let desc = if self.counts {
                    Value::Global(desc_name(*f, args.len()))
                } else {
                    Value::Null
                };
                self.b.store_at(Type::Ptr, desc, &p, 16);
                for ((a, t), o) in args.iter().zip(&tys).zip(offsets) {
                    let v = self.value(a)?;
                    self.b.store_at(ty(t)?, v, &p, o);
                }
                self.bind(binds, p)?;
            }
            Op::Apply => {
                let Ty::Fn(sig, _) = self.atom_ty(&args[0]) else {
                    return Err("apply of something that isn't a closure".into());
                };
                let ret = ret_ty(&sig.results)?;
                let (fp, all) = self.apply_args(args)?;
                self.bind_results(Call::tailcc(ret, fp, all), binds)?;
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
                    self.reuse(&args[0], total, c.index)?
                } else {
                    // Without counts, a reuse token holds no memory.
                    self.alloc(total, c.index)
                };
                for ((a, t), o) in fields.iter().zip(&tys).zip(offsets) {
                    let v = self.value(a)?;
                    self.b.store_at(ty(t)?, v, &p, o);
                }
                self.bind(binds, p)?;
            }
            Op::Const(c) => {
                let decl = self
                    .m
                    .consts
                    .get(c)
                    .ok_or_else(|| format!("unknown constant {c}"))?;
                let v = match &decl.value {
                    ConstValue::Lit(l) => lit(l),
                    _ => Value::Global(const_name(*c)),
                };
                self.bind(binds, v)?;
            }
            Op::Get(c, i) | Op::Set(c, i) => {
                let tys = ctor_fields(self.m, *c)?;
                let (offsets, _) = layout(&tys);
                let (t, o) = (&tys[*i as usize], offsets[*i as usize]);
                let p = self.value(&args[0])?;
                if matches!(op, Op::Get(..)) {
                    let x = self.b.load_at(ty(t)?, &p, o);
                    self.bind(binds, x)?;
                } else {
                    let v = self.value(&args[1])?;
                    self.replace_at(t, v, &p, o)?;
                }
            }
            Op::ArrayNew => {
                let e = self.elem(self.f.var_ty(binds[0]))?;
                let p = self.new_array(&Value::from(args.len()), size(&e));
                for (k, a) in args.iter().enumerate() {
                    let v = self.value(a)?;
                    let o = 8 + k as u64 * size(&e);
                    self.b.store_at(ty(&e)?, v, &p, o);
                }
                self.bind(binds, p)?;
            }
            Op::ArrayFill | Op::ArrayAlloc => {
                let e = self.elem(self.f.var_ty(binds[0]))?;
                let n = self.value(&args[0])?;
                let p = self.new_array(&n, size(&e));
                // The elements that aren't set yet are null, or 0.
                if *op == Op::ArrayAlloc {
                    let bytes = elem_offset(&mut self.b, &n, size(&e));
                    let elems = self.b.bin(BinOp::Sub, Type::I64, bytes, 8);
                    let to = self.b.offset(&p, 8);
                    let args = vec![
                        Type::Ptr.val(to),
                        Type::I8.val(0),
                        Type::I64.val(elems),
                        Type::I1.val(0),
                    ];
                    let memset = Value::global("llvm.memset.p0.i64");
                    self.b.call(Call::c(Type::Void, memset, args));
                }
                if *op == Op::ArrayFill {
                    let v = self.value(&args[1])?;
                    let (t, stride) = (ty(&e)?, size(&e));
                    count_loop(&mut self.b, &n, |b, k| {
                        let o = elem_offset(b, k, stride);
                        b.store_at(t, &v, &p, o);
                    });
                    // It takes one reference, and the array holds `n`.
                    if self.counts && Kind::of(&e).is_some() {
                        self.add_count(&v, &n, &e);
                    }
                }
                self.bind(binds, p)?;
            }
            Op::ArrayGet | Op::ArraySet => {
                let e = self.elem(&self.atom_ty(&args[0]))?;
                let a = self.value(&args[0])?;
                let i = self.value(&args[1])?;
                self.check_index(&a, &i);
                let o = elem_offset(&mut self.b, &i, size(&e));
                if *op == Op::ArrayGet {
                    let x = self.b.load_at(ty(&e)?, &a, o);
                    self.bind(binds, x)?;
                } else {
                    let v = self.value(&args[2])?;
                    self.replace_at(&e, v, &a, o)?;
                }
            }
            Op::ArrayLoad | Op::ArrayStore => {
                let t = match op {
                    Op::ArrayLoad => self.f.var_ty(binds[0]).clone(),
                    _ => self.atom_ty(&args[2]),
                };
                let a = self.value(&args[0])?;
                let at = self.value(&args[1])?;
                self.check_bytes(&a, &at, size(&t));
                let o = elem_offset(&mut self.b, &at, 1);
                let q = self.b.offset(&a, o);
                if *op == Op::ArrayLoad {
                    let x = self.b.load_unaligned(ty(&t)?, q);
                    self.bind(binds, x)?;
                } else {
                    let v = self.value(&args[2])?;
                    self.b.store_unaligned(ty(&t)?, v, q);
                }
            }
            Op::ArrayLen => {
                let a = self.value(&args[0])?;
                let n = self.b.load_at(Type::I32, &a, 4);
                self.bind(binds, n)?;
            }
            Op::ArrayCopy => self.array_copy(args)?,
            Op::ArrayClone => {
                let e = self.elem(&self.atom_ty(&args[0]))?;
                let a = self.value(&args[0])?;
                let n = self.b.load_at(Type::I32, &a, 4);
                let p = self.new_array(&n, size(&e));
                let bytes = elem_offset(&mut self.b, &n, size(&e));
                let elems = self.b.bin(BinOp::Sub, Type::I64, bytes, 8);
                let (to, from) = (self.b.offset(&p, 8), self.b.offset(&a, 8));
                self.memmove(to, from, elems);
                // The clone shares the elements.
                if self.counts && Kind::of(&e).is_some() {
                    self.each(&p, &Value::Int(0), &n, Value::global("conduit_dup"));
                }
                self.bind(binds, p)?;
            }
            // The same array, now `imm`.
            Op::ArrayFreeze => {
                let a = self.value(&args[0])?;
                self.bind(binds, a)?;
            }
            Op::RefNew => {
                let t = self.atom_ty(&args[0]);
                let p = self.alloc(16, 0);
                let v = self.value(&args[0])?;
                self.b.store_at(ty(&t)?, v, &p, 8);
                self.bind(binds, p)?;
            }
            // An `any` holds the descriptor of the kind of what it holds,
            // its drop and visit functions, if it's counted.
            Op::Box(t) => {
                let p = self.alloc(24, 0);
                let v = self.value(&args[0])?;
                self.b.store_at(ty(t)?, v, &p, 8);
                let desc = match Kind::of(t) {
                    Some(k) if self.counts => {
                        let desc = k.desc();
                        self.kinds.insert(k);
                        desc
                    }
                    _ => Value::Null,
                };
                self.b.store_at(Type::Ptr, desc, &p, 16);
                self.bind(binds, p)?;
            }
            Op::RefGet | Op::Unbox(_) => {
                let t = self.f.var_ty(binds[0]).clone();
                let p = self.value(&args[0])?;
                let x = self.b.load_at(ty(&t)?, &p, 8);
                self.bind(binds, x)?;
            }
            Op::RefSet => {
                let Ty::Ref(t) = self.atom_ty(&args[0]) else {
                    return Err("ref_set of something that isn't a ref".into());
                };
                let p = self.value(&args[0])?;
                let v = self.value(&args[1])?;
                self.replace_at(&t, v, &p, 8)?;
            }
            // Nothing to store: whatever the slot holds is what it gives,
            // as nothing may use it, counting included. But a reuse token
            // that's undefined holds no memory, which `reuse` checks for.
            Op::Undefined => {
                if let Some(&b) = binds.first()
                    && matches!(self.f.var_ty(b), Ty::Reuse(_))
                {
                    self.store(b, Value::Null)?;
                }
            }
            // Without counts, nothing is freed: counting does nothing, no
            // object is unique, and a reuse token holds no memory.
            // Gives up a reference without counting.
            Op::Forget => {}
            Op::Dup | Op::Drop | Op::Free | Op::DecRef if !self.counts => {}
            Op::IsUnique if !self.counts => self.bind(binds, 0)?,
            Op::DropReuse | Op::ReuseToken if !self.counts => self.bind(binds, Value::Null)?,
            Op::Dup | Op::Drop => {
                let t = self.atom_ty(&args[0]);
                let Some(k) = Kind::of(&t) else {
                    return Ok(());
                };
                let x = self.value(&args[0])?;
                let f = if *op == Op::Dup {
                    Value::global("conduit_dup")
                } else {
                    self.drop_of(&t, k)
                };
                call_void(&mut self.b, f, vec![Type::Ptr.val(x)]);
            }
            Op::IsUnique => {
                let x = self.value(&args[0])?;
                let r = self.call_on(Type::I32, "conduit_is_unique", &x);
                self.bind(binds, r)?;
            }
            Op::Free => {
                let x = self.value(&args[0])?;
                call_void(
                    &mut self.b,
                    Value::global("conduit_free"),
                    vec![Type::Ptr.val(x)],
                );
            }
            // What isn't `imm` may be left in a garbage cycle.
            Op::DecRef => {
                let t = self.atom_ty(&args[0]);
                let x = Type::Ptr.val(self.value(&args[0])?);
                match Kind::of(&t) {
                    Some(k) if self.cycles && !counts::is_imm(&t) => {
                        let args = vec![x, Type::Ptr.val(k.visit_fn())];
                        self.kinds.insert(k);
                        call_void(&mut self.b, Value::global("conduit_decref_buf"), args);
                    }
                    _ => call_void(&mut self.b, Value::global("conduit_decref"), vec![x]),
                }
            }
            // At 0, what it held is dropped and its memory is the token,
            // unless the collector has it: then the token holds none. It's
            // taken first, since dropping what it held may collect.
            Op::DropReuse => {
                let t = self.atom_ty(&args[0]);
                let Ty::Data(d, ..) = t else {
                    return Err("drop_reuse of something that isn't data".into());
                };
                let k = Kind::Data(d);
                let x = self.value(&args[0])?;
                let last = if self.cycles && !counts::is_imm(&t) {
                    let args = vec![Type::Ptr.val(&x), Type::Ptr.val(k.visit_fn())];
                    let last_buf = Value::global("conduit_last_buf");
                    self.b.call_value(Call::tailcc(Type::I1, last_buf, args))
                } else {
                    self.call_on(Type::I1, "conduit_last", &x)
                };
                self.kinds.insert(k);
                let slot = self.b.alloca(Type::Ptr);
                self.b.store(Type::Ptr, Value::Null, &slot);
                let (free, done) = (self.b.new_block(), self.b.new_block());
                self.b.cond_br(last, &free, &done);
                self.b.start(&free);
                let taken = self.call_on(Type::Ptr, "conduit_take", &x);
                self.b.store(Type::Ptr, taken, &slot);
                call_void(&mut self.b, counts::children_fn(d), vec![Type::Ptr.val(&x)]);
                self.b.br(&done);
                self.b.start(&done);
                let token = self.b.load(Type::Ptr, &slot);
                self.bind(binds, token)?;
            }
            Op::ReuseToken => {
                let x = self.value(&args[0])?;
                let token = self.call_on(Type::Ptr, "conduit_take", &x);
                self.bind(binds, token)?;
            }
            // A null host value is a null box.
            Op::HostNull => self.bind(binds, Value::Null)?,
            Op::HostIsNull => {
                let x = self.value(&args[0])?;
                let null = self.b.icmp(IntPred::Eq, Type::Ptr, x, Value::Null);
                let r = self.b.cast(CastOp::ZExt, Type::I1, null, Type::I32);
                self.bind(binds, r)?;
            }
            // The same box, with the same reference.
            Op::HostUnwrap => {
                let x = self.value(&args[0])?;
                let null = self.b.icmp(IntPred::Eq, Type::Ptr, &x, Value::Null);
                self.trap_if(null);
                self.bind(binds, x)?;
            }
            // The host makes a function of the trampoline for the
            // closure's signature and the closure, whose reference it
            // takes, and gives it back as a host value.
            Op::HostFunction => {
                let Ty::Fn(sig, _) = self.atom_ty(&args[0]) else {
                    return Err("host_function of something that isn't a closure".into());
                };
                let k = (self.callbacks.iter())
                    .position(|(p, r)| *p == sig.params && *r == sig.results)
                    .ok_or("no trampoline for a host function")?;
                let c = self.value(&args[0])?;
                let tramp = Value::Global(trampoline_name(k));
                let make = Value::global("conduit_host_function");
                let pair = (self.b).call_value(Call::c(
                    host_pair(),
                    make,
                    vec![Type::Ptr.val(tramp), Type::Ptr.val(c)],
                ));
                let value = self.b.extract_value(host_pair(), &pair, 0);
                let ops = self.b.extract_value(host_pair(), &pair, 1);
                let h = host_in(&mut self.b, self.counts, value, ops, false);
                self.bind(binds, h)?;
            }
            other => return Err(format!("{other:?} isn't supported yet")),
        }
        Ok(())
    }

    /// What the runtime's function `name` gives, of type `ret`, on the
    /// object `x`.
    fn call_on(&mut self, ret: Type, name: &str, x: &Value) -> Value {
        let call = Call::tailcc(ret, Value::global(name), vec![Type::Ptr.val(x)]);
        self.b.call_value(call)
    }

    /// The drop of a value of type `t`, of kind `k`: with cycles, one that
    /// never buffers it if it's `imm`.
    fn drop_of(&mut self, t: &Ty, k: Kind) -> Value {
        let f = if self.cycles && counts::is_imm(t) {
            k.drop_imm_fn()
        } else {
            k.drop_fn()
        };
        self.kinds.insert(k);
        f
    }

    /// Stores `v`, of type `t`, at `off` in `p`, and drops what it
    /// replaces, when counting.
    fn replace_at(
        &mut self,
        t: &Ty,
        v: Value,
        p: &Value,
        off: impl Into<Value>,
    ) -> Result<(), String> {
        let (lt, off) = (ty(t)?, off.into());
        match Kind::of(t) {
            Some(k) if self.counts => {
                let old = self.b.load_at(lt.clone(), p, &off);
                self.b.store_at(lt, v, p, off);
                let drop = self.drop_of(t, k);
                call_void(&mut self.b, drop, vec![Type::Ptr.val(old)]);
            }
            _ => self.b.store_at(lt, v, p, off),
        }
        Ok(())
    }

    /// Gives `v` the `n - 1` more references an array of `n` copies of it
    /// holds, or drops it if `n` is 0.
    fn add_count(&mut self, v: &Value, n: &Value, e: &Ty) {
        let k = Kind::of(e).expect("a counted element");
        let none = self.b.icmp(IntPred::Eq, Type::I32, n, 0);
        let (zero, some, done) = (self.b.new_block(), self.b.new_block(), self.b.new_block());
        self.b.cond_br(none, &zero, &some);
        self.b.start(&zero);
        let drop = self.drop_of(e, k);
        call_void(&mut self.b, drop, vec![Type::Ptr.val(v)]);
        self.b.br(&done);
        self.b.start(&some);
        let more = self.b.bin(BinOp::Sub, Type::I32, n, 1);
        let args = vec![Type::Ptr.val(v), Type::I32.val(more)];
        call_void(&mut self.b, Value::global("conduit_add"), args);
        self.b.br(&done);
        self.b.start(&done);
    }

    /// Calls `f` on each of the `n` elements of array `a` from `start`, all
    /// `i32` values, which are objects.
    fn each(&mut self, a: &Value, start: &Value, n: &Value, f: Value) {
        count_loop(&mut self.b, n, |b, k| {
            let at = b.bin(BinOp::Add, Type::I32, start, k);
            let o = elem_offset(b, at, 8);
            let x = b.load_at(Type::Ptr, a, o);
            call_void(b, f, vec![Type::Ptr.val(x)]);
        });
    }

    /// An object of `size` bytes in the memory of `token`, if it holds
    /// some, or a new one: either way with its count at 1 and `word` at 4.
    fn reuse(
        &mut self,
        token: &Atom,
        size: impl Into<Value>,
        word: impl Into<Value>,
    ) -> Result<Value, String> {
        let (size, word) = (size.into(), word.into());
        let t = self.value(token)?;
        let out = self.b.alloca(Type::Ptr);
        let none = self.b.icmp(IntPred::Eq, Type::Ptr, &t, Value::Null);
        let (fresh, reused, done) = (self.b.new_block(), self.b.new_block(), self.b.new_block());
        self.b.cond_br(none, &fresh, &reused);
        self.b.start(&fresh);
        let p = self.alloc(size, &word);
        self.b.store(Type::Ptr, p, &out);
        self.b.br(&done);
        self.b.start(&reused);
        self.b.store(Type::I32, 1, &t);
        self.b.store_at(Type::I32, word, &t, 4);
        self.b.store(Type::Ptr, &t, &out);
        self.b.br(&done);
        self.b.start(&done);
        Ok(self.b.load(Type::Ptr, &out))
    }

    fn memmove(&mut self, to: Value, from: Value, bytes: Value) {
        let args = vec![
            Type::Ptr.val(to),
            Type::Ptr.val(from),
            Type::I64.val(bytes),
            Type::I1.val(0),
        ];
        let f = Value::global("llvm.memmove.p0.p0.i64");
        self.b.call(Call::c(Type::Void, f, args));
    }

    /// `array_copy(dst, dst_start, src, src_start, count)`: as `memmove`,
    /// trapping unless both ranges fit, read as unsigned.
    fn array_copy(&mut self, args: &[Atom]) -> Result<(), String> {
        let e = self.elem(&self.atom_ty(&args[0]))?;
        let stride = size(&e);
        let vals: Vec<Value> = args
            .iter()
            .map(|a| self.value(a))
            .collect::<Result<_, _>>()?;
        let (dst, ds, src, ss, n) = (&vals[0], &vals[1], &vals[2], &vals[3], &vals[4]);
        let n64 = self.b.cast(CastOp::ZExt, Type::I32, n, Type::I64);
        for (a, start) in [(dst, ds), (src, ss)] {
            let len = self.b.load_at(Type::I32, a, 4);
            let s64 = self.b.cast(CastOp::ZExt, Type::I32, start, Type::I64);
            let l64 = self.b.cast(CastOp::ZExt, Type::I32, len, Type::I64);
            let end = self.b.bin(BinOp::Add, Type::I64, s64, &n64);
            let out = self.b.icmp(IntPred::Ugt, Type::I64, end, l64);
            self.trap_if(out);
        }
        // The copies are references more, and what they overwrite one less.
        if self.counts
            && let Some(k) = Kind::of(&e)
        {
            self.each(src, ss, n, Value::global("conduit_dup"));
            let drop = self.drop_of(&e, k);
            self.each(dst, ds, n, drop);
        }
        let to = elem_offset(&mut self.b, ds, stride);
        let from = elem_offset(&mut self.b, ss, stride);
        let (to, from) = (self.b.offset(dst, to), self.b.offset(src, from));
        let bytes = self.b.bin(BinOp::Mul, Type::I64, n64, stride);
        self.memmove(to, from, bytes);
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
        if ext.abi == Abi::Flat {
            return self.call_flat(symbol, ext, args, binds);
        }
        // A host value goes out as the host's value, which it borrows, and
        // comes back with its ops, in a new box.
        let results = &ext.sig.results;
        let (ret, out_tys) = c_results(results)?;
        let mut out = Vec::with_capacity(args.len() + out_tys.len());
        for (a, t) in args.iter().zip(&ext.sig.params) {
            let v = self.value(a)?;
            let v = if t.is_host() {
                host_out(&mut self.b, &v, *t == Ty::NullableHost)
            } else {
                v
            };
            out.push(Typed(ty(t)?, v));
        }
        // Where the callee writes each result after the first.
        let slots: Vec<(Type, Value)> = (out_tys.into_iter())
            .map(|t| (t.clone(), self.b.alloca(t)))
            .collect();
        out.extend(slots.iter().map(|(_, p)| Type::Ptr.val(p)));
        let r = self.call_out(Call::c(ret, Value::global(&**symbol), out));
        let mut values: Vec<Value> = r.into_iter().collect();
        for (t, p) in slots {
            values.push(self.b.load(t, p));
        }
        for ((t, v), &b) in results.iter().zip(values).zip(binds) {
            let v = if t.is_host() {
                let value = self.b.extract_value(host_pair(), &v, 0);
                let ops = self.b.extract_value(host_pair(), &v, 1);
                host_in(&mut self.b, self.counts, value, ops, *t == Ty::NullableHost)
            } else {
                v
            };
            self.store(b, v)?;
        }
        Ok(())
    }

    /// A call of `#[abi(flat)]` extern `ext`, as [`extern_sig`] says. The
    /// iovecs of a list come from `calloc`, and are freed after the call;
    /// the out-parameters are `alloca`s.
    fn call_flat(
        &mut self,
        symbol: &str,
        ext: &conduit_ir::Extern,
        args: &[Atom],
        binds: &[Var],
    ) -> Result<(), String> {
        let (ret, _) = extern_sig(ext)?;
        let mut out = Vec::new();
        let mut lists = Vec::new();
        for (i, (a, t)) in args.iter().zip(&ext.sig.params).enumerate() {
            let v = self.value(a)?;
            match t {
                Ty::Array(e, _) if matches!(**e, Ty::Array(..)) => {
                    let n = self.b.load_at(Type::I32, &v, 4);
                    let iov = self.iovecs(&v, &n);
                    out.push(Typed(Type::Ptr, iov.clone()));
                    out.push(Typed(Type::I32, n));
                    lists.push(iov);
                }
                Ty::Array(..) => {
                    let elems = self.b.offset(&v, 8);
                    out.push(Typed(Type::Ptr, elems));
                    if !ext.addr(i) {
                        let n = self.b.load_at(Type::I32, &v, 4);
                        out.push(Typed(Type::I32, n));
                    }
                }
                Ty::I8 | Ty::I16 => {
                    let wide = self.b.cast(CastOp::ZExt, ty(t)?, v, Type::I32);
                    out.push(Typed(Type::I32, wide));
                }
                t => out.push(Typed(ty(t)?, v)),
            }
        }
        let mut outs = Vec::new();
        for t in ext.sig.results.iter().skip(1) {
            let t = ty(t)?;
            let p = self.b.alloca(t.clone());
            out.push(Typed(Type::Ptr, p.clone()));
            outs.push((t, p));
        }
        let r = self.call_out(Call::c(ret, Value::global(symbol), out));
        for iov in lists {
            let free = Call::c(
                Type::Void,
                Value::global("free"),
                vec![Typed(Type::Ptr, iov)],
            );
            self.b.call(free);
        }
        if let (Some(r), Some(&b)) = (r, binds.first()) {
            self.store(b, r)?;
        }
        for ((t, p), &b) in outs.into_iter().zip(binds.iter().skip(1)) {
            let x = self.b.load(t, p);
            self.store(b, x)?;
        }
        Ok(())
    }

    /// A `struct iovec`, `{void *base, size_t len}`, for each of the `n`
    /// byte buffers of list `l`, an `i32` value, in memory from `calloc`
    /// that the caller frees. Traps if there's no memory left.
    fn iovecs(&mut self, l: &Value, n: &Value) -> Value {
        let count = self.b.cast(CastOp::ZExt, Type::I32, n, Type::I64);
        let args = vec![Type::I64.val(count), Type::I64.val(16)];
        let iov = (self.b).call_value(Call::c(Type::Ptr, Value::global("calloc"), args));
        // `calloc` may give null for no buffers, which the callee doesn't read.
        let null = self.b.icmp(IntPred::Eq, Type::Ptr, &iov, Value::Null);
        let some = self.b.icmp(IntPred::Ne, Type::I32, n, 0);
        let failed = self.b.bin(BinOp::And, Type::I1, null, some);
        self.trap_if(failed);
        count_loop(&mut self.b, n, |b, k| {
            let e = elem_offset(b, k, 8);
            let buf = b.load_at(Type::Ptr, l, e);
            let base = b.offset(&buf, 8);
            let len = b.load_at(Type::I32, &buf, 4);
            let len = b.cast(CastOp::ZExt, Type::I32, len, Type::I64);
            let k = b.cast(CastOp::ZExt, Type::I32, k, Type::I64);
            let at = b.bin(BinOp::Mul, Type::I64, k, 16);
            b.store_at(Type::Ptr, base, &iov, &at);
            let at8 = b.bin(BinOp::Add, Type::I64, at, 8);
            b.store_at(Type::I64, len, &iov, at8);
        });
        iov
    }

    /// The block of join point `l`.
    fn join_block(l: Label) -> ir::Label {
        ir::Label(format!("j{}", l.0))
    }

    fn tail(&mut self, t: &Tail) -> Result<(), String> {
        match t {
            Tail::Join { defs, body, .. } => {
                self.block(body)?;
                for d in defs {
                    self.b.start(&Self::join_block(d.label));
                    self.block(&d.body)?;
                }
            }
            Tail::Jump {
                target,
                args,
                source,
            } if *target == Label::RET => {
                self.at(*source);
                self.ret(args)?
            }
            Tail::Jump {
                target,
                args,
                source,
            } => {
                self.at(*source);
                let def = (self.defs.get(target.0 as usize).copied().flatten())
                    .ok_or_else(|| format!("{target} isn't defined"))?;
                // Every argument first, since they may be the parameters.
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.value(a)?);
                }
                for (p, v) in def.params.iter().zip(values) {
                    self.store(*p, v)?;
                }
                self.b.br(&Self::join_block(*target));
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                let st = ty(&self.atom_ty(scrut))?;
                let s = self.value(scrut)?;
                // A case on data is on its constructor, at 4.
                let (t, s) = if st == Type::Ptr {
                    (Type::I32, self.b.load_at(Type::I32, &s, 4))
                } else {
                    (st, s)
                };
                let labels: Vec<ir::Label> = arms.iter().map(|_| self.b.new_block()).collect();
                let other = self.b.new_block();
                let cases = (arms.iter().zip(&labels))
                    .map(|(arm, label)| {
                        let n = match arm.tag {
                            Tag::Int(n) => n,
                            Tag::Ctor(c) => c.index as i64,
                        };
                        (n, label.clone())
                    })
                    .collect();
                self.b.switch(t.val(s), &other, cases);
                for (arm, label) in arms.iter().zip(&labels) {
                    self.b.start(label);
                    self.block(&arm.body)?;
                }
                self.b.start(&other);
                match default {
                    Some(d) => self.block(d)?,
                    None => self.b.unreachable(),
                }
            }
            Tail::Unreachable => self.b.unreachable(),
            Tail::Trap => trap(&mut self.b),
        }
        Ok(())
    }

    fn ret(&mut self, args: &[Atom]) -> Result<(), String> {
        let ret = self.ret.clone();
        match args {
            [] => self.b.ret_void(),
            [a] => {
                let v = self.value(a)?;
                self.b.ret(Some(ret.val(v)));
            }
            many => {
                let mut agg = Value::Undef;
                for (i, a) in many.iter().enumerate() {
                    let t = ty(&self.atom_ty(a))?;
                    let v = self.value(a)?;
                    agg = self.b.insert_value(ret.clone(), agg, t.val(v), i as u32);
                }
                self.b.ret(Some(ret.val(agg)));
            }
        }
        Ok(())
    }

    /// Branches to a trap if `cond` holds, and goes on in a new block.
    fn trap_if(&mut self, cond: Value) {
        trap_if(&mut self.b, cond);
    }

    fn prim(&mut self, p: &Prim, args: &[Atom]) -> Result<Value, String> {
        let at = self.atom_ty(&args[0]);
        let t = ty(&at)?;
        let float = matches!(at, Ty::F32 | Ty::F64);
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.value(a)?);
        }
        let bin = |me: &mut Self, op: BinOp| me.b.bin(op, t.clone(), &vals[0], &vals[1]);
        let suffix = if at == Ty::F32 { "f32" } else { "f64" };
        let intrinsic = |name: &str| Value::Global(format!("llvm.{name}.{suffix}"));
        Ok(match p {
            Prim::Add => bin(self, if float { BinOp::FAdd } else { BinOp::Add }),
            Prim::Sub => bin(self, if float { BinOp::FSub } else { BinOp::Sub }),
            Prim::Mul => bin(self, if float { BinOp::FMul } else { BinOp::Mul }),
            Prim::Div(_) if float => bin(self, BinOp::FDiv),
            Prim::Div(sign) | Prim::Rem(sign) => {
                let div = matches!(p, Prim::Div(_));
                let signed = *sign == Sign::Signed;
                let (x, y) = (&vals[0], &vals[1]);
                let z = self.b.icmp(IntPred::Eq, t.clone(), y, 0);
                // A signed remainder by -1 is 0, which LLVM leaves undefined
                // for the smallest value; a signed division by -1 of it traps.
                let minus_one = self.b.icmp(IntPred::Eq, t.clone(), y, -1);
                if div && signed {
                    let smallest = i64::MIN >> (64 - int_bits(&at));
                    let min = self.b.icmp(IntPred::Eq, t.clone(), x, smallest);
                    let ov = self.b.bin(BinOp::And, Type::I1, min, &minus_one);
                    self.trap_if(ov);
                }
                let safe_cond = if !div && signed {
                    self.b.bin(BinOp::Or, Type::I1, &z, &minus_one)
                } else {
                    z.clone()
                };
                let d = self.b.select(safe_cond, t.clone(), 1, y);
                let op = match (div, signed) {
                    (true, true) => BinOp::SDiv,
                    (true, false) => BinOp::UDiv,
                    (false, true) => BinOp::SRem,
                    (false, false) => BinOp::URem,
                };
                let q = self.b.bin(op, t.clone(), x, d);
                if div {
                    self.b.select(z, t, 0, q)
                } else {
                    let q = if signed {
                        self.b.select(minus_one, t.clone(), 0, q)
                    } else {
                        q
                    };
                    self.b.select(z, t, x, q)
                }
            }
            Prim::Neg if float => self.b.fneg(t, &vals[0]),
            Prim::Neg => self.b.bin(BinOp::Sub, t, 0, &vals[0]),
            Prim::And => bin(self, BinOp::And),
            Prim::Or => bin(self, BinOp::Or),
            Prim::Xor => bin(self, BinOp::Xor),
            Prim::Shl | Prim::Shr(_) => {
                let mask = int_bits(&at) - 1;
                let n = self.b.bin(BinOp::And, t.clone(), &vals[1], mask);
                let op = match p {
                    Prim::Shl => BinOp::Shl,
                    Prim::Shr(Sign::Signed) => BinOp::AShr,
                    _ => BinOp::LShr,
                };
                self.b.bin(op, t, &vals[0], n)
            }
            Prim::Eq | Prim::Ne | Prim::Lt(_) | Prim::Le(_) | Prim::Gt(_) | Prim::Ge(_) => {
                let (x, y) = (&vals[0], &vals[1]);
                let b = if float {
                    let pred = match p {
                        Prim::Eq => FloatPred::Oeq,
                        Prim::Ne => FloatPred::Une,
                        Prim::Lt(_) => FloatPred::Olt,
                        Prim::Le(_) => FloatPred::Ole,
                        Prim::Gt(_) => FloatPred::Ogt,
                        _ => FloatPred::Oge,
                    };
                    self.b.fcmp(pred, t, x, y)
                } else {
                    let s = |sign: &Sign| *sign == Sign::Signed;
                    let pred = match p {
                        Prim::Eq => IntPred::Eq,
                        Prim::Ne => IntPred::Ne,
                        Prim::Lt(sg) if s(sg) => IntPred::Slt,
                        Prim::Lt(_) => IntPred::Ult,
                        Prim::Le(sg) if s(sg) => IntPred::Sle,
                        Prim::Le(_) => IntPred::Ule,
                        Prim::Gt(sg) if s(sg) => IntPred::Sgt,
                        Prim::Gt(_) => IntPred::Ugt,
                        Prim::Ge(sg) if s(sg) => IntPred::Sge,
                        _ => IntPred::Uge,
                    };
                    self.b.icmp(pred, t, x, y)
                };
                self.b.cast(CastOp::ZExt, Type::I1, b, Type::I32)
            }
            Prim::Abs | Prim::Sqrt => {
                let f = intrinsic(if *p == Prim::Abs { "fabs" } else { "sqrt" });
                let args = vec![t.val(&vals[0])];
                self.b.call_value(Call::c(t, f, args))
            }
            Prim::Min | Prim::Max => {
                let f = intrinsic(if *p == Prim::Min {
                    "minimum"
                } else {
                    "maximum"
                });
                let args = vec![t.val(&vals[0]), t.val(&vals[1])];
                self.b.call_value(Call::c(t, f, args))
            }
            Prim::Convert(to, sign) => self.convert(&at, to, *sign, &vals[0])?,
            Prim::Reinterpret(to) => self.b.cast(CastOp::Bitcast, t, &vals[0], ty(to)?),
        })
    }

    fn convert(&mut self, from: &Ty, to: &Ty, sign: Sign, x: &Value) -> Result<Value, String> {
        let (f, t) = (ty(from)?, ty(to)?);
        let float = |t: &Ty| matches!(t, Ty::F32 | Ty::F64);
        let op = match (float(from), float(to)) {
            (false, false) => match int_bits(from).cmp(&int_bits(to)) {
                std::cmp::Ordering::Equal => return Ok(x.clone()),
                std::cmp::Ordering::Greater => CastOp::Trunc,
                std::cmp::Ordering::Less if sign == Sign::Signed => CastOp::SExt,
                std::cmp::Ordering::Less => CastOp::ZExt,
            },
            (false, true) if sign == Sign::Signed => CastOp::SIToFP,
            (false, true) => CastOp::UIToFP,
            (true, true) if from == to => return Ok(x.clone()),
            (true, true) if *from == Ty::F32 => CastOp::FPExt,
            (true, true) => CastOp::FPTrunc,
            (true, false) => return Ok(self.float_to_int(from, to, sign, x)),
        };
        Ok(self.b.cast(op, f, x, t))
    }

    /// A float converted to an integer, rounding toward 0, which traps if
    /// it's NaN or out of the integer's range, as in Wasm. It converts when
    /// it's above the smallest integer minus one and below the largest plus
    /// one, or from the smallest itself, a power of 2, where the smallest
    /// minus one isn't a value of the float type. NaN fails every ordered
    /// comparison, so it traps too.
    fn float_to_int(&mut self, from: &Ty, to: &Ty, sign: Sign, x: &Value) -> Value {
        let (f, t) = (ty(from).unwrap(), ty(to).unwrap());
        let bits = int_bits(to) as i32;
        let as_from = |v: f64| -> (Value, bool) {
            if *from == Ty::F32 {
                (lit(&Lit::f32(v as f32)), f64::from(v as f32) == v)
            } else {
                (lit(&Lit::f64(v)), true)
            }
        };
        let (lo_pred, lo, hi) = match sign {
            Sign::Signed => {
                let min = -(2f64.powi(bits - 1));
                let hi = as_from(2f64.powi(bits - 1)).0;
                match as_from(min - 1.0) {
                    (lo, true) if (min - 1.0) != min => (FloatPred::Ogt, lo, hi),
                    _ => (FloatPred::Oge, as_from(min).0, hi),
                }
            }
            Sign::Unsigned => (FloatPred::Ogt, as_from(-1.0).0, as_from(2f64.powi(bits)).0),
        };
        let above = self.b.fcmp(lo_pred, f.clone(), x, lo);
        let below = self.b.fcmp(FloatPred::Olt, f.clone(), x, hi);
        let ok = self.b.bin(BinOp::And, Type::I1, above, below);
        let bad = self.b.bin(BinOp::Xor, Type::I1, ok, 1);
        self.trap_if(bad);
        let op = if sign == Sign::Signed {
            CastOp::FPToSI
        } else {
            CastOp::FPToUI
        };
        self.b.cast(op, f, x, t)
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

#[cfg(test)]
mod tests {
    fn emit(src: &str) -> Result<String, String> {
        let m = conduit_ir::text::parse(src).map_err(|e| e.to_string())?;
        crate::emit_module(&m).map_err(|e| e.to_string())
    }

    #[test]
    fn externs_share_a_symbol() {
        let text = emit(
            "#[link(c, symbol = \"f\")]\nextern a(i32) -> (i32)\n\
             #[link(c, symbol = \"f\")]\nextern b(i32) -> (i32)\n",
        )
        .unwrap();
        assert_eq!(text.matches("declare i32 @f(").count(), 1, "{text}");
        let e = emit(
            "#[link(c, symbol = \"f\")]\nextern a(i32) -> (i32)\n\
             #[link(c, symbol = \"f\")]\nextern b(i64) -> (i32)\n",
        )
        .unwrap_err();
        assert!(
            e.contains("both link to `f`, with different signatures"),
            "{e}"
        );
    }

    #[test]
    fn several_results_go_through_pointers() {
        // As a struct of all three, C would return this one through a
        // hidden pointer on x86-64 and arm64, and LLVM in registers.
        let text = emit(
            "#[link(c, symbol = \"g\")]\n\
             extern g(i32) -> (i64, host, i32)\n\
             #[export(c, symbol = \"h\")]\n\
             fn f(%x: i32) -> (i32, i64) {\n  jump ret(%x, 1i64)\n}\n",
        )
        .unwrap();
        assert!(text.contains("declare i64 @g(i32, ptr, ptr)"), "{text}");
        assert!(text.contains("define i32 @h(i32 %p0, ptr %r1)"), "{text}");
    }

    #[test]
    fn flat_externs_take_pointers_and_lengths() {
        let text = emit(
            "#[link(c, symbol = \"g\")]\n#[abi(flat)]\n\
             extern g(imm [imm [i8]], #[addr] mut [i32], i8) -> (i32, i64)\n",
        )
        .unwrap();
        assert!(
            text.contains("declare i32 @g(ptr, i32, ptr, i32, ptr)"),
            "{text}"
        );
    }
}
