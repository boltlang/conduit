//! Lowers Conduit IR to WebAssembly with objects in linear memory, so the
//! output needs no GC: only tail calls and bulk memory, both part of Wasm
//! 2.0 or 3.0. Memory is never freed yet: every object comes from
//! `alloc`, a bump allocator, so that freeing can come later by replacing
//! it.
//!
//! Effect handlers whose clauses are all tail-resumptive or aborting (see
//! [`conduit_ir::clause`]) are lowered directly, as the GC backend does: a
//! global holds a stack of handler frames in memory, `perform` finds the
//! nearest frame of its effect and calls its clause through the table, and
//! an aborting clause throws an exception, from exception handling, part
//! of Wasm 3.0. A module with any other clause is lowered by evidence
//! passing first ([`conduit_opt::effects`]), so that it needs no stack
//! switching.
//!
//! A handler frame is the header, its effect's number at 8, the next frame
//! at 12, a table slot for each clause's wrapper from 16, then the
//! handler's environment, then, for a handler with an aborting clause, its
//! results, which that clause sets before it throws.
//!
//! The layouts, with every offset in bytes and every object 8-aligned:
//!
//! - **A data object** is a header word, kept for a reference count or a
//!   mark, the index of its constructor at 4, and its fields from 8, each
//!   at its natural alignment.
//! - **An array** is the header, its length at 4, and its elements from 8,
//!   `i8` and `i16` packed.
//! - **A closure** is the header, the table index of its wrapper at 4, and
//!   what it captured from 8, laid out like fields. The wrapper takes the
//!   closure, then the arguments, as in the GC backend.
//! - **A ref** and **a boxed scalar** are the header, then the value at 8.
//!   A boxed reference is the reference itself.
//!
//! A reference is an `i32` address, and 0 is never one, so `undefined` is 0.
//! Fresh memory is zero, which is what unset array elements are.
//! Constants are laid out when the module is built, in a data segment
//! starting at 16, and the heap starts after them.

mod func;
mod switch;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use conduit_ir::verify::Item;
use conduit_ir::{
    ConstId, ConstValue, CtorId, EffectId, ExternId, Func, FuncId, HandlerId, Lit, Module, Ty,
};
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, ElementSection, Elements, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    MemorySection, MemoryType, NameMap, NameSection, RefType, TableSection, TableType, TagKind,
    TagSection, TagType, TypeSection, ValType,
};

use crate::Error;

/// Where constants start: 0 is never an object, and the rest of the first
/// bytes are kept free.
const CONST_BASE: u32 = 16;

/// The global holding the next free address.
const HEAP: u32 = 0;

/// The global holding the innermost handler frame, or 0.
const HANDLERS: u32 = 1;

/// The global holding the handler stack of the `perform` whose clause is
/// running, under which a `discontinue` in that clause performs.
const SITE: u32 = 2;

/// The exception an aborting clause throws, holding its handler's frame.
const ABORT: u32 = 0;

/// How a value is stored in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Storage {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl Storage {
    fn of(ty: &Ty) -> Storage {
        match ty {
            Ty::I8 => Storage::I8,
            Ty::I16 => Storage::I16,
            Ty::I64 => Storage::I64,
            Ty::F32 => Storage::F32,
            Ty::F64 => Storage::F64,
            // An `i32`, or a reference.
            _ => Storage::I32,
        }
    }

    fn size(self) -> u32 {
        match self {
            Storage::I8 => 1,
            Storage::I16 => 2,
            Storage::I32 | Storage::F32 => 4,
            Storage::I64 | Storage::F64 => 8,
        }
    }
}

/// The value type of a Conduit type: a reference is an `i32` address.
pub(crate) fn val(ty: &Ty) -> ValType {
    match ty {
        Ty::I64 => ValType::I64,
        Ty::F32 => ValType::F32,
        Ty::F64 => ValType::F64,
        _ => ValType::I32,
    }
}

/// The offsets of values of these types laid out from 8, and the size of
/// the whole object.
pub(crate) fn layout(tys: &[Ty]) -> (Vec<(u32, Storage)>, u32) {
    let mut at: u32 = 8;
    let mut fields = Vec::with_capacity(tys.len());
    for ty in tys {
        let s = Storage::of(ty);
        at = at.next_multiple_of(s.size());
        fields.push((at, s));
        at += s.size();
    }
    (fields, at.next_multiple_of(8))
}

/// A function signature after lowering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FnSig {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// A type of the type section: a function type, or a continuation of one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TypeDef {
    Func(FnSig),
    /// A continuation of the function type at this index.
    Cont(u32),
}

/// State shared by the functions of a module.
pub(crate) struct Cx<'m> {
    pub m: &'m Module,
    funcs: BTreeMap<FuncId, u32>,
    externs: BTreeMap<ExternId, u32>,
    /// The index of `alloc`.
    pub alloc: u32,
    /// The wrappers closures call, by function and number of captures,
    /// in the order of their table slots.
    entries: Vec<(FuncId, usize)>,
    entry_keys: HashMap<(FuncId, usize), u32>,
    /// The types, by what they are. A continuation type comes after its
    /// function type, which it refers to.
    types: Vec<TypeDef>,
    type_keys: HashMap<TypeDef, u32>,
    /// The address of each constant that's an object.
    consts: BTreeMap<ConstId, u32>,
    /// For each handler, the table slot of its first clause's wrapper. The
    /// wrappers come first in the table.
    wrappers: BTreeMap<HandlerId, u32>,
    /// How many table slots the wrappers take, before the closures'.
    wrapper_slots: u32,
    /// The handlers with an aborting clause, and those clauses.
    aborting: BTreeSet<HandlerId>,
    aborts: BTreeSet<FuncId>,
    /// Whether some clause discontinues, which needs [`SITE`].
    pub discontinues: bool,
    /// The functions of each handler that switches stacks.
    switches: BTreeMap<HandlerId, Switch>,
    /// The control tag of each operation of an effect that a handler that
    /// switches stacks handles.
    tags: BTreeMap<(EffectId, u32), u32>,
    /// The exception `discontinue` throws into a continuation: the address
    /// of a closure that performs the operation.
    pub disc_tag: Option<u32>,
    /// The function `discontinue E.i` performs with.
    thunks: BTreeMap<(EffectId, u32), FuncId>,
}

/// The functions, table slots and tables of a handler that switches
/// stacks. See `switch.rs`.
#[derive(Clone)]
pub(crate) struct Switch {
    pub start: u32,
    pub tramp: u32,
    /// For each operation: resumes its continuation, throws into it, the
    /// table slots of its continuation's wrapper and discontinue function,
    /// and the table of its Wasm continuations.
    pub drive: Vec<u32>,
    pub throw: Vec<u32>,
    pub kont_slots: Vec<u32>,
    pub disc_slots: Vec<u32>,
    pub conts: Vec<u32>,
    kont_funcs: Vec<u32>,
    disc_funcs: Vec<u32>,
}

/// How [`emit_module_with`] lowers a module.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Whether to lower handlers with a clause that's neither
    /// tail-resumptive nor aborting with the stack switching proposal,
    /// which isn't standard yet and only some engines run, such as
    /// Wasmtime on x86-64 Linux and macOS. Otherwise, a module with such a
    /// handler is lowered by evidence passing, which runs anywhere but
    /// costs more.
    pub stack_switching: bool,
}

impl Cx<'_> {
    pub fn func_index(&self, f: FuncId) -> Result<u32, String> {
        self.funcs
            .get(&f)
            .copied()
            .ok_or_else(|| format!("unknown function {f}"))
    }

    pub fn extern_index(&self, e: ExternId) -> Result<u32, String> {
        self.externs
            .get(&e)
            .copied()
            .ok_or_else(|| format!("unknown extern {e}"))
    }

    /// The type index of a signature.
    pub fn sig(&mut self, sig: FnSig) -> u32 {
        self.type_def(TypeDef::Func(sig))
    }

    /// The type index of a continuation resumed with `sig.params` that
    /// gives `sig.results`.
    pub fn cont(&mut self, sig: FnSig) -> u32 {
        let f = self.sig(sig);
        self.type_def(TypeDef::Cont(f))
    }

    fn type_def(&mut self, t: TypeDef) -> u32 {
        if let Some(&i) = self.type_keys.get(&t) {
            return i;
        }
        self.types.push(t.clone());
        let i = self.types.len() as u32 - 1;
        self.type_keys.insert(t, i);
        i
    }

    /// The type of the wrappers of closures of signature `sig`:
    /// `(closure, params..) -> results`.
    pub fn entry_sig(&mut self, sig: &conduit_ir::Sig) -> u32 {
        let mut params = vec![ValType::I32];
        params.extend(sig.params.iter().map(val));
        let results = sig.results.iter().map(val).collect();
        self.sig(FnSig { params, results })
    }

    /// The table slot of the wrapper of `f`'s closures with `n` captures.
    pub fn entry(&mut self, f: FuncId, n: usize) -> u32 {
        if let Some(&k) = self.entry_keys.get(&(f, n)) {
            return self.wrapper_slots + k;
        }
        let k = self.entries.len() as u32;
        self.entries.push((f, n));
        self.entry_keys.insert((f, n), k);
        self.wrapper_slots + k
    }

    /// The number of an effect, which its frames hold.
    pub fn effect_number(&self, e: EffectId) -> Result<i32, String> {
        self.m
            .effects
            .keys()
            .position(|&k| k == e)
            .map(|i| i as i32 + 1)
            .ok_or_else(|| format!("unknown effect {e}"))
    }

    /// The table slot of the wrapper of clause `i` of `h`.
    pub fn wrapper(&self, h: HandlerId, i: u32) -> Result<u32, String> {
        self.wrappers
            .get(&h)
            .map(|first| first + i)
            .ok_or_else(|| format!("{h} has a clause that isn't tail-resumptive or aborting"))
    }

    pub fn switch(&self, h: HandlerId) -> Result<&Switch, String> {
        self.switches
            .get(&h)
            .ok_or_else(|| format!("{h} doesn't switch stacks"))
    }

    pub fn is_switching(&self, h: HandlerId) -> bool {
        self.switches.contains_key(&h)
    }

    /// The control tag of operation `i` of `e`, if a handler of `e`
    /// switches stacks.
    pub fn tag(&self, e: EffectId, i: u32) -> Option<u32> {
        self.tags.get(&(e, i)).copied()
    }

    /// The function `discontinue e.i` performs with.
    pub fn thunk(&self, e: EffectId, i: u32) -> Result<FuncId, String> {
        self.thunks
            .get(&(e, i))
            .copied()
            .ok_or_else(|| format!("no function to discontinue with {e}.{i}"))
    }

    pub fn is_aborting(&self, h: HandlerId) -> bool {
        self.aborting.contains(&h)
    }

    pub fn is_abort(&self, clause: FuncId) -> bool {
        self.aborts.contains(&clause)
    }

    /// The type of the wrappers of clauses for operation `i` of `e`:
    /// `(frame, params..) -> results`.
    pub fn clause_sig(&mut self, e: EffectId, i: u32) -> Result<u32, String> {
        let op = self
            .m
            .effects
            .get(&e)
            .and_then(|d| d.ops.get(i as usize))
            .ok_or_else(|| format!("{e} has no operation {i}"))?;
        let mut params = vec![ValType::I32];
        params.extend(op.params.iter().map(val));
        let results = op.results.iter().map(val).collect();
        Ok(self.sig(FnSig { params, results }))
    }

    /// The layout of a frame of `h`: its fields' offsets and its size. The
    /// fields are the effect's number, the next frame, a slot per clause,
    /// the environment, and the results if `h` has an aborting clause.
    pub fn frame(&self, h: HandlerId) -> Result<(Vec<(u32, Storage)>, u32), String> {
        let decl = self
            .m
            .handlers
            .get(&h)
            .ok_or_else(|| format!("unknown handler {h}"))?;
        let mut tys = vec![Ty::I32, Ty::I32];
        tys.extend(std::iter::repeat_n(Ty::I32, decl.ops.len()));
        tys.extend(decl.env.iter().cloned());
        if self.aborting.contains(&h) {
            let ret = self
                .m
                .funcs
                .get(&decl.ret)
                .ok_or_else(|| format!("unknown return clause {}", decl.ret))?;
            tys.extend(ret.results.iter().cloned());
        }
        Ok(layout(&tys))
    }

    /// The fields of constructor `c`.
    pub fn fields(&self, c: CtorId) -> Result<&[Ty], String> {
        self.m
            .data
            .get(&c.data)
            .and_then(|d| d.ctors.get(c.index as usize))
            .map(|ctor| &ctor.fields[..])
            .ok_or_else(|| format!("unknown constructor {c}"))
    }

    /// The address of constant `c`, if it's an object, or else its value.
    pub fn constant(&self, c: ConstId) -> Result<Result<u32, Lit>, String> {
        if let Some(&a) = self.consts.get(&c) {
            return Ok(Ok(a));
        }
        match conduit_ir::verify::const_value(self.m, c) {
            Some(ConstValue::Lit(l)) => Ok(Err(*l)),
            _ => Err(format!("unknown constant {c}")),
        }
    }
}

/// Lowers a module that has passed [`conduit_ir::verify::verify_module`]
/// to a Wasm binary with objects in linear memory, exporting each function
/// in `exports` under its name, and the memory as `memory`, with the
/// default [`Options`]: without stack switching.
pub fn emit_module(m: &Module, exports: &[(FuncId, &str)]) -> Result<Vec<u8>, Error> {
    emit_module_with(m, exports, &Options::default())
}

/// [`emit_module`] with `options`. A module with a handler whose clauses
/// aren't all tail-resumptive or aborting is lowered with stack switching
/// if the options ask for it, and by evidence passing first otherwise,
/// which keeps every function's ID.
pub fn emit_module_with(
    m: &Module,
    exports: &[(FuncId, &str)],
    options: &Options,
) -> Result<Vec<u8>, Error> {
    let lowered;
    let mut thunks = BTreeMap::new();
    let m = if !options.stack_switching && !direct_handlers(m) {
        let mut copy = m.clone();
        conduit_opt::effects::lower_effects(&mut copy).map_err(|message| Error {
            item: Item::Func(FuncId(0)),
            message: format!("lowering effects: {message}"),
        })?;
        lowered = copy;
        &lowered
    } else if options.stack_switching && !direct_handlers(m) {
        let (copy, map) = with_thunks(m);
        lowered = copy;
        thunks = map;
        &lowered
    } else {
        m
    };
    let switching: BTreeSet<HandlerId> = m
        .handlers
        .keys()
        .copied()
        .filter(|&h| !direct_handler(m, h))
        .collect();

    // Tail-resumptive and aborting clauses are lowered as plain functions,
    // without their continuation.
    let mut plain_clauses: BTreeMap<FuncId, Func> = BTreeMap::new();
    let mut aborts = BTreeSet::new();
    let mut aborting = BTreeSet::new();
    let mut discontinues = false;
    for (&h, decl) in &m.handlers {
        if switching.contains(&h) {
            continue;
        }
        for &clause in &decl.ops {
            let Some(f) = m.funcs.get(&clause) else {
                continue;
            };
            if let Some(plain) = conduit_ir::clause::plain_call(f) {
                discontinues |= conduit_ir::clause::discontinues(&plain);
                plain_clauses.insert(clause, plain);
            } else if let Some(plain) = conduit_ir::clause::aborting(f) {
                plain_clauses.insert(clause, plain);
                aborts.insert(clause);
                aborting.insert(h);
            }
        }
    }
    let mut wrappers = BTreeMap::new();
    let mut wrapper_slots = 0;
    for (&h, decl) in &m.handlers {
        if !switching.contains(&h) {
            wrappers.insert(h, wrapper_slots);
            wrapper_slots += decl.ops.len() as u32;
        }
    }
    let clause_slots = wrapper_slots;

    let first = m.externs.len() as u32;
    let mut cx = Cx {
        m,
        externs: m.externs.keys().zip(0..).map(|(&e, i)| (e, i)).collect(),
        funcs: m.funcs.keys().zip(first..).map(|(&f, i)| (f, i)).collect(),
        alloc: first + m.funcs.len() as u32,
        entries: Vec::new(),
        entry_keys: HashMap::new(),
        types: Vec::new(),
        type_keys: HashMap::new(),
        consts: BTreeMap::new(),
        wrappers,
        wrapper_slots,
        aborting,
        aborts,
        discontinues,
        switches: BTreeMap::new(),
        tags: BTreeMap::new(),
        disc_tag: None,
        thunks,
    };
    let mut switch_funcs = 0;
    if !switching.is_empty() {
        switch_funcs = plan_switching(&mut cx, &switching).map_err(|message| Error {
            item: Item::Handler(*switching.first().unwrap()),
            message,
        })?;
    }

    // Constants, laid out from CONST_BASE.
    let mut data = vec![0u8; CONST_BASE as usize];
    for &c in m.consts.keys() {
        let err = |message| Error {
            item: Item::Const(c),
            message,
        };
        if let Some(addr) = lay_out_const(&mut cx, &mut data, c).map_err(err)? {
            cx.consts.insert(c, addr);
        }
    }
    let heap_base = (data.len() as u32).next_multiple_of(8);
    let data = data[CONST_BASE as usize..].to_vec();

    let mut imports = Vec::new();
    for (&id, ext) in &m.externs {
        let sig = FnSig {
            params: ext.sig.params.iter().map(val).collect(),
            results: ext.sig.results.iter().map(val).collect(),
        };
        let _ = id;
        imports.push((ext, cx.sig(sig)));
    }

    let mut funcs = Vec::new();
    for (&id, f) in &m.funcs {
        let err = |message| Error {
            item: Item::Func(id),
            message,
        };
        let f = plain_clauses.get(&id).unwrap_or(f);
        let sig = FnSig {
            params: f.params.iter().map(|&p| val(f.var_ty(p))).collect(),
            results: f.results.iter().map(val).collect(),
        };
        let ty = cx.sig(sig);
        let body = func::lower_func(&mut cx, f).map_err(err)?;
        funcs.push((ty, body));
    }

    let alloc_ty = cx.sig(FnSig {
        params: vec![ValType::I32],
        results: vec![ValType::I32],
    });
    let alloc = func::lower_alloc();

    // The wrappers of clauses, which `perform` calls through the table.
    let mut clause_wrappers = Vec::new();
    for (&h, decl) in &m.handlers {
        if switching.contains(&h) {
            continue;
        }
        for i in 0..decl.ops.len() as u32 {
            let err = |message| Error {
                item: Item::Handler(h),
                message,
            };
            let ty = cx.clause_sig(decl.effect, i).map_err(err)?;
            let clause = decl.ops[i as usize];
            let plain = plain_clauses
                .get(&clause)
                .ok_or_else(|| err(format!("clause {clause} isn't tail-resumptive or aborting")))?;
            let body = func::lower_wrapper(&mut cx, h, i, plain).map_err(err)?;
            clause_wrappers.push((ty, body));
        }
    }
    // The functions of handlers that switch stacks, in the order
    // `plan_switching` numbered them.
    let mut switched = Vec::new();
    for &h in &switching {
        let err = |message| Error {
            item: Item::Handler(h),
            message,
        };
        let n = m.effects[&m.handlers[&h].effect].ops.len() as u32;
        let disc = cx.disc_tag.is_some();
        let sig = cx.drive_sig(h, switch::Start::Body).map_err(err)?;
        let body = switch::lower_drive(&mut cx, h, switch::Start::Body).map_err(err)?;
        switched.push((cx.sig(sig), body));
        let tramp_sig = FnSig {
            params: vec![ValType::I32],
            results: cx.body_results(h).map_err(err)?,
        };
        let body = switch::lower_tramp(&mut cx, h).map_err(err)?;
        switched.push((cx.sig(tramp_sig), body));
        for i in 0..n {
            let start = switch::Start::Resume(i);
            let sig = cx.drive_sig(h, start).map_err(err)?;
            let body = switch::lower_drive(&mut cx, h, start).map_err(err)?;
            switched.push((cx.sig(sig), body));
        }
        let results = cx.handle_results(h).map_err(err)?;
        for i in 0..n {
            let op = &m.effects[&m.handlers[&h].effect].ops[i as usize];
            let ksig = conduit_ir::Sig::new(
                op.results.clone(),
                m.funcs[&m.handlers[&h].ret].results.clone(),
            );
            let ty = cx.entry_sig(&ksig);
            let body = switch::lower_kont(&mut cx, h, i, false).map_err(err)?;
            switched.push((ty, body));
        }
        if disc {
            for i in 0..n {
                let start = switch::Start::Throw(i);
                let sig = cx.drive_sig(h, start).map_err(err)?;
                let body = switch::lower_drive(&mut cx, h, start).map_err(err)?;
                switched.push((cx.sig(sig), body));
            }
            for i in 0..n {
                let ty = cx.disc_sig(results.clone());
                let body = switch::lower_kont(&mut cx, h, i, true).map_err(err)?;
                switched.push((ty, body));
            }
        }
    }
    // Tag types, after the abort's.
    let disc_tag_ty = cx.disc_tag.map(|_| {
        cx.sig(FnSig {
            params: vec![ValType::I32],
            results: Vec::new(),
        })
    });
    let mut control_tag_tys = Vec::new();
    for &(e, i) in cx.tags.clone().keys() {
        let op = &m.effects[&e].ops[i as usize];
        control_tag_tys.push(cx.sig(FnSig {
            params: op.params.iter().map(val).collect(),
            results: op.results.iter().map(val).collect(),
        }));
    }
    // The tables of Wasm continuations, after the function table.
    let mut cont_tables = Vec::new();
    for &h in &switching {
        let n = m.effects[&m.handlers[&h].effect].ops.len() as u32;
        for i in 0..n {
            let err = |message| Error {
                item: Item::Handler(h),
                message,
            };
            cont_tables.push(cx.op_cont(h, i).map_err(err)?);
        }
    }

    let abort_ty = (!cx.aborts.is_empty()).then(|| {
        cx.sig(FnSig {
            params: vec![ValType::I32],
            results: Vec::new(),
        })
    });

    let mut entries = Vec::new();
    let mut k = 0;
    while k < cx.entries.len() {
        let (f, n) = cx.entries[k];
        let err = |message| Error {
            item: Item::Func(f),
            message,
        };
        let func = &m.funcs[&f];
        let sig = func.sig();
        let rest = conduit_ir::Sig::new(sig.params[n..].to_vec(), sig.results.clone());
        let ty = cx.entry_sig(&rest);
        let body = func::lower_entry(&mut cx, f, n).map_err(err)?;
        entries.push((ty, body));
        k += 1;
    }

    let mut types = TypeSection::new();
    for t in &cx.types {
        match t {
            TypeDef::Func(sig) => {
                types
                    .ty()
                    .function(sig.params.iter().copied(), sig.results.iter().copied());
            }
            TypeDef::Cont(f) => {
                types.ty().subtype(&wasm_encoder::SubType {
                    is_final: true,
                    supertype_idxs: Vec::new(),
                    composite_type: wasm_encoder::CompositeType {
                        inner: wasm_encoder::CompositeInnerType::Cont(wasm_encoder::ContType(*f)),
                        shared: false,
                        descriptor: None,
                        describes: None,
                    },
                });
            }
        }
    }
    let mut import_section = ImportSection::new();
    for (ext, sig) in &imports {
        import_section.import(&ext.module, &ext.name, EntityType::Function(*sig));
    }
    let mut functions = FunctionSection::new();
    let mut code = CodeSection::new();
    let bodies = funcs
        .iter()
        .map(|(ty, b)| (*ty, b))
        .chain(std::iter::once((alloc_ty, &alloc)))
        .chain(clause_wrappers.iter().map(|(ty, b)| (*ty, b)))
        .chain(switched.iter().map(|(ty, b)| (*ty, b)))
        .chain(entries.iter().map(|(ty, b)| (*ty, b)));
    for (ty, body) in bodies {
        functions.function(ty);
        let mut f = Function::new_with_locals_types(body.locals.iter().copied());
        for i in &body.code {
            f.instruction(i);
        }
        code.function(&f);
    }

    let mut module = wasm_encoder::Module::new();
    module.section(&types);
    module.section(&import_section);
    module.section(&functions);
    // Always a table, even an empty one, since a `perform` with no handler
    // in the module still calls through it, after trapping.
    let n_entries = u64::from(cx.wrapper_slots) + cx.entries.len() as u64;
    {
        let mut tables = TableSection::new();
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: n_entries,
            maximum: Some(n_entries),
            shared: false,
        });
        for &ct in &cont_tables {
            tables.table(TableType {
                element_type: RefType {
                    nullable: true,
                    heap_type: wasm_encoder::HeapType::Concrete(ct),
                },
                table64: false,
                minimum: 0,
                maximum: None,
                shared: false,
            });
        }
        module.section(&tables);
    }
    let pages = u64::from(heap_base.div_ceil(65536)).max(1);
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: pages,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&memories);
    // The abort's tag first, then the one `discontinue` throws, then the
    // control tags.
    let tag_tys: Vec<u32> = abort_ty
        .into_iter()
        .chain(disc_tag_ty)
        .chain(control_tag_tys)
        .collect();
    if !tag_tys.is_empty() {
        let mut tags = TagSection::new();
        for ty in tag_tys {
            tags.tag(TagType {
                kind: TagKind::Exception,
                func_type_idx: ty,
            });
        }
        module.section(&tags);
    }
    let mut globals = GlobalSection::new();
    // The heap pointer, the handler stack, and the stack of the `perform`
    // whose clause runs.
    for init in [heap_base as i32, 0, 0] {
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(init),
        );
    }
    module.section(&globals);
    let mut export_section = ExportSection::new();
    for &(f, name) in exports {
        let idx = cx.func_index(f).map_err(|message| Error {
            item: Item::Func(f),
            message,
        })?;
        export_section.export(name, ExportKind::Func, idx);
    }
    export_section.export("memory", ExportKind::Memory, 0);
    module.section(&export_section);
    // The function of each table slot: the clauses' wrappers, the
    // continuations' wrappers and discontinue functions, then the
    // closures' wrappers, which come after every other function.
    let mut refs: Vec<u32> = (0..clause_slots).map(|s| cx.alloc + 1 + s).collect();
    for sw in cx.switches.values() {
        refs.extend(sw.kont_funcs.iter().copied());
        refs.extend(sw.disc_funcs.iter().copied());
    }
    let first_entry = cx.alloc + 1 + clause_slots + switch_funcs;
    refs.extend((0..cx.entries.len() as u32).map(|k| first_entry + k));
    let mut elements = ElementSection::new();
    if !refs.is_empty() {
        elements.active(
            Some(0),
            &ConstExpr::i32_const(0),
            Elements::Functions(refs.into()),
        );
    }
    // The trampolines, which `ref.func` names.
    let tramps: Vec<u32> = cx.switches.values().map(|sw| sw.tramp).collect();
    if !tramps.is_empty() {
        elements.declared(Elements::Functions(tramps.into()));
    }
    if !elements.is_empty() {
        module.section(&elements);
    }
    module.section(&code);
    if !data.is_empty() {
        let mut segments = DataSection::new();
        segments.active(0, &ConstExpr::i32_const(CONST_BASE as i32), data);
        module.section(&segments);
    }
    if let Some(names) = name_section(m, &cx) {
        module.section(&names);
    }
    Ok(module.finish())
}

/// Whether every clause of every handler of `m` is tail-resumptive or
/// aborting, so that its handlers can be lowered directly.
fn direct_handlers(m: &Module) -> bool {
    m.handlers.keys().all(|&h| direct_handler(m, h))
}

/// Whether every clause of `h` is tail-resumptive or aborting.
fn direct_handler(m: &Module, h: HandlerId) -> bool {
    m.handlers[&h].ops.iter().all(|c| {
        m.funcs.get(c).is_some_and(|f| {
            conduit_ir::clause::plain_call(f).is_some() || conduit_ir::clause::aborting(f).is_some()
        })
    })
}

/// `m` with a function for each operation some function discontinues a
/// continuation with: it takes the operation's arguments and performs it.
fn with_thunks(m: &Module) -> (Module, BTreeMap<(EffectId, u32), FuncId>) {
    let mut ops = BTreeSet::new();
    for f in m.funcs.values() {
        let mut todo = vec![&f.body];
        while let Some(b) = todo.pop() {
            for l in &b.lets {
                if let (conduit_ir::Op::Discontinue(e, i), Some(conduit_ir::Atom::Var(k))) =
                    (&l.op, l.args.first())
                    && matches!(f.vars.get(k.0 as usize), Some(Ty::Cont(_)))
                {
                    ops.insert((*e, *i));
                }
            }
            match &b.tail {
                conduit_ir::Tail::Join { defs, body, .. } => {
                    todo.extend(defs.iter().map(|d| &d.body));
                    todo.push(body);
                }
                conduit_ir::Tail::Case { arms, default, .. } => {
                    todo.extend(arms.iter().map(|a| &a.body));
                    todo.extend(default.as_deref());
                }
                _ => {}
            }
        }
    }
    let mut out = m.clone();
    let mut map = BTreeMap::new();
    let first = m.funcs.keys().map(|f| f.0 + 1).max().unwrap_or(0);
    for (next, (e, i)) in (first..).zip(ops) {
        let op = &m.effects[&e].ops[i as usize];
        let mut f = Func::new(op.params.clone(), op.results.clone());
        f.row = conduit_ir::Row::one(e);
        let results: Vec<_> = op.results.iter().map(|t| f.new_var(t.clone())).collect();
        let args = f.params.iter().map(|&p| conduit_ir::Atom::Var(p)).collect();
        f.body = conduit_ir::Block {
            lets: vec![conduit_ir::Let {
                binds: results.clone(),
                op: conduit_ir::Op::Perform(e, i),
                args,
                source: None,
            }],
            tail: conduit_ir::Tail::Jump {
                target: conduit_ir::Label::RET,
                args: results.into_iter().map(conduit_ir::Atom::Var).collect(),
                source: None,
            },
        };
        f.debug.name = Some("discontinue".into());
        out.funcs.insert(FuncId(next), f);
        map.insert((e, i), FuncId(next));
    }
    (out, map)
}

/// Numbers the functions, table slots, tables and tags of the handlers in
/// `switching`, after the clauses' wrappers, in the order `emit_module_with`
/// lowers them. Returns how many functions they take.
fn plan_switching(cx: &mut Cx<'_>, switching: &BTreeSet<HandlerId>) -> Result<u32, String> {
    let disc = !cx.thunks.is_empty();
    let first_func = cx.alloc + 1 + cx.wrapper_slots;
    let mut func = first_func;
    let mut slot = cx.wrapper_slots;
    let mut table = 1;
    let take = |next: &mut u32, n: u32| {
        let range: Vec<u32> = (*next..*next + n).collect();
        *next += n;
        range
    };
    for &h in switching {
        let n = cx.m.effects[&cx.m.handlers[&h].effect].ops.len() as u32;
        let start = take(&mut func, 1)[0];
        let tramp = take(&mut func, 1)[0];
        let drive = take(&mut func, n);
        let kont_funcs = take(&mut func, n);
        let (throw, disc_funcs) = if disc {
            (take(&mut func, n), take(&mut func, n))
        } else {
            (Vec::new(), Vec::new())
        };
        let kont_slots = take(&mut slot, n);
        let disc_slots = if disc { take(&mut slot, n) } else { Vec::new() };
        let conts = take(&mut table, n);
        cx.switches.insert(
            h,
            Switch {
                start,
                tramp,
                drive,
                throw,
                kont_slots,
                disc_slots,
                conts,
                kont_funcs,
                disc_funcs,
            },
        );
    }
    cx.wrapper_slots = slot;
    let mut tag = u32::from(!cx.aborts.is_empty());
    if disc {
        cx.disc_tag = Some(tag);
        tag += 1;
    }
    let effects: BTreeSet<EffectId> = switching.iter().map(|h| cx.m.handlers[h].effect).collect();
    for e in effects {
        for i in 0..cx.m.effects[&e].ops.len() as u32 {
            cx.tags.insert((e, i), tag);
            tag += 1;
        }
    }
    Ok(func - first_func)
}

/// Lays out constant `c` in `data`, after what's there, and gives its
/// address, or `None` if it's a scalar.
fn lay_out_const(cx: &mut Cx<'_>, data: &mut Vec<u8>, c: ConstId) -> Result<Option<u32>, String> {
    if let Some(&a) = cx.consts.get(&c) {
        return Ok(Some(a));
    }
    let decl =
        cx.m.consts
            .get(&c)
            .ok_or_else(|| format!("unknown constant {c}"))?;
    match &decl.value {
        ConstValue::Lit(_) => Ok(None),
        value => {
            let ty = decl.ty.clone();
            let a = lay_out_value(cx, data, value, &ty)?;
            cx.consts.insert(c, a);
            Ok(Some(a))
        }
    }
}

/// Lays out an object of a constant, and gives its address.
fn lay_out_value(
    cx: &mut Cx<'_>,
    data: &mut Vec<u8>,
    value: &ConstValue,
    ty: &Ty,
) -> Result<u32, String> {
    // What goes in a slot of type `ty`: a literal's bits, or an address.
    let slot =
        |cx: &mut Cx<'_>, data: &mut Vec<u8>, v: &ConstValue, ty: &Ty| -> Result<Vec<u8>, String> {
            Ok(match v {
                ConstValue::Lit(l) => lit_bytes(*l),
                ConstValue::Const(id) => match lay_out_const(cx, data, *id)? {
                    Some(a) => a.to_le_bytes().to_vec(),
                    None => match conduit_ir::verify::const_value(cx.m, *id) {
                        Some(ConstValue::Lit(l)) => lit_bytes(*l),
                        _ => return Err(format!("unknown constant {id}")),
                    },
                },
                nested => lay_out_value(cx, data, nested, ty)?.to_le_bytes().to_vec(),
            })
        };
    match (value, ty) {
        (ConstValue::Array(elems), Ty::Array(..)) => {
            let elem = conduit_ir::verify::elem_ty(ty).unwrap();
            let size = Storage::of(&elem).size();
            let mut bytes = vec![0u8; 8 + size as usize * elems.len()];
            bytes[4..8].copy_from_slice(&(elems.len() as u32).to_le_bytes());
            for (i, e) in elems.iter().enumerate() {
                let b = slot(cx, data, e, &elem)?;
                let at = 8 + i * size as usize;
                bytes[at..at + size as usize].copy_from_slice(&b[..size as usize]);
            }
            Ok(place(data, bytes))
        }
        (ConstValue::New(c, fields), Ty::Data(_, m, args, rows)) => {
            let decl = cx.fields(*c)?.to_vec();
            let tys: Vec<Ty> = decl
                .iter()
                .map(|f| conduit_ir::verify::resolve(f, *m, args, rows))
                .collect();
            let (offsets, size) = layout(&tys);
            let mut bytes = vec![0u8; size as usize];
            bytes[4..8].copy_from_slice(&c.index.to_le_bytes());
            for ((v, t), (at, s)) in fields.iter().zip(&tys).zip(&offsets) {
                let b = slot(cx, data, v, t)?;
                let at = *at as usize;
                bytes[at..at + s.size() as usize].copy_from_slice(&b[..s.size() as usize]);
            }
            Ok(place(data, bytes))
        }
        (ConstValue::Const(id), _) => {
            lay_out_const(cx, data, *id)?.ok_or_else(|| format!("{id} isn't an object"))
        }
        _ => Err(format!("a constant's value {value:?} isn't a {ty}")),
    }
}

/// Puts an object's bytes at the next 8-aligned address of `data`.
fn place(data: &mut Vec<u8>, bytes: Vec<u8>) -> u32 {
    let at = data.len().next_multiple_of(8);
    data.resize(at, 0);
    data.extend(bytes);
    at as u32
}

/// A literal's bytes, little-endian, at least as many as its storage.
fn lit_bytes(l: Lit) -> Vec<u8> {
    match l {
        Lit::I8(n) => vec![n as u8],
        Lit::I16(n) => n.to_le_bytes().to_vec(),
        Lit::I32(n) => n.to_le_bytes().to_vec(),
        Lit::I64(n) => n.to_le_bytes().to_vec(),
        Lit::F32(bits) => bits.to_le_bytes().to_vec(),
        Lit::F64(bits) => bits.to_le_bytes().to_vec(),
    }
}

/// Names for tools and stack traces: the externs', functions' and
/// `alloc`'s.
fn name_section(m: &Module, cx: &Cx<'_>) -> Option<NameSection> {
    let mut functions = NameMap::new();
    for (i, ext) in m.externs.values().enumerate() {
        if let Some(name) = &ext.debug_name {
            functions.append(i as u32, name);
        }
    }
    let first = m.externs.len() as u32;
    for (i, f) in m.funcs.values().enumerate() {
        if let Some(name) = &f.debug.name {
            functions.append(first + i as u32, name);
        }
    }
    functions.append(cx.alloc, "alloc");
    let mut section = NameSection::new();
    section.functions(&functions);
    Some(section)
}

#[cfg(test)]
mod tests;
