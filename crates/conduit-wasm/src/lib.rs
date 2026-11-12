//! Lowers Conduit IR to WebAssembly, with objects in linear memory, so
//! the output needs no GC: only tail calls and bulk memory, and exception
//! handling for aborting handlers, all part of Wasm 3.0. See
//! `docs/wasm.md`. Every object comes from
//! `alloc`. By default that's a bump allocator and memory is never freed;
//! with [`Options::counts`], for a module whose reference counts are
//! explicit, objects are freed onto free lists that `alloc` takes from
//! (see `counts.rs`).
//!
//! Effect handlers whose clauses are all tail-resumptive or aborting (see
//! [`conduit_ir::clause`]) are lowered directly: a
//! global holds a stack of handler frames in memory, `perform` finds the
//! nearest frame of a handler of its effect and calls its clause through
//! the table, and
//! an aborting clause throws an exception, from exception handling, part
//! of Wasm 3.0. A module with any other clause is lowered by evidence
//! passing first ([`conduit_effects`]), so that it needs no stack
//! switching.
//!
//! A handler frame is the header, its handler's number at 8, the next frame
//! at 12, a table slot for each clause's wrapper from 16, the clauses of
//! each effect it handles in turn, then the
//! handler's environment, then, for a handler with an aborting clause, its
//! results, which that clause sets before it throws.
//!
//! The layouts, with every offset in bytes and every object 8-aligned:
//!
//! - **A data object** is a header word, its reference count when
//!   counting, the index of its constructor at 4, and its fields from 8,
//!   each at its natural alignment.
//! - **An array** is the header, its length at 4, and its elements from 8,
//!   `i8` and `i16` packed.
//! - **A closure** is the header, the table index of its wrapper at 4, and
//!   what it captured from 8, laid out like fields. The wrapper takes the
//!   closure, then the arguments.
//! - **A ref** and **a boxed scalar** are the header, then the value at 8.
//!   A boxed reference is the reference itself, except when counting:
//!   then every `any` is a box, with the kind of what it holds at 4, so
//!   that dropping it can drop what it holds.
//!
//! A reference is an `i32` address, and 0 is never one, so `undefined` is 0.
//! Fresh memory is zero, which is what unset array elements are.
//! Constants are laid out when the module is built, in a data segment
//! starting at 16, and the heap starts after them.

mod counts;
mod escape;
mod func;
mod shadow;
mod switch;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use conduit_ir::verify::Item;
use conduit_ir::{
    Abi, ConstId, ConstValue, CtorId, EffectId, Export, ExternId, Func, FuncId, HandlerId, Link,
    Lit, Module, Ty,
};
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, ElementSection, Elements, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    Instruction as I, MemorySection, MemoryType, NameMap, NameSection, RefType, TableSection,
    TableType, TagKind, TagSection, TagType, TypeSection, ValType,
};

use std::fmt;

/// Why a module couldn't be lowered.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Error {
    pub item: Item,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.item, self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    /// What makes a message an error in `item`, for `map_err`.
    fn at(item: Item) -> impl Fn(String) -> Error {
        move |message| Error { item, message }
    }
}

/// Where constants start: 0 is never an object, and the rest of the first
/// bytes are kept free.
const CONST_BASE: u32 = 16;

/// The global holding the next free address.
const HEAP: u32 = 0;

/// The global holding the innermost handler frame, or 0.
const HANDLERS: u32 = 1;

/// The global holding the handler stack of the `perform` whose clause is
/// running, under which a `discontinue` in that clause performs.
pub(crate) const SITE: u32 = 2;

/// How a value is stored in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

impl FnSig {
    pub fn new(
        params: impl IntoIterator<Item = ValType>,
        results: impl IntoIterator<Item = ValType>,
    ) -> FnSig {
        FnSig {
            params: params.into_iter().collect(),
            results: results.into_iter().collect(),
        }
    }
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
    /// How many of those the clauses' wrappers take, which come first.
    clause_slots: u32,
    /// The handlers with an aborting clause, and those clauses.
    aborting: BTreeSet<HandlerId>,
    aborts: BTreeSet<FuncId>,
    /// Whether some clause discontinues, which needs [`SITE`].
    pub discontinues: bool,
    /// The functions of each handler that switches stacks, and how many
    /// there are of them.
    switches: BTreeMap<HandlerId, Switch>,
    switch_funcs: u32,
    /// The control tag of each operation of an effect that a handler that
    /// switches stacks handles.
    tags: BTreeMap<(EffectId, u32), u32>,
    /// The exception `discontinue` throws into a continuation: the address
    /// of a closure that performs the operation.
    pub disc_tag: Option<u32>,
    /// With counts, the exception thrown into a continuation that's freed
    /// without being used, so that its stack unwinds, dropping what its
    /// frames own: it has no value.
    pub cancel_tag: Option<u32>,
    /// The function `discontinue E.i` performs with.
    thunks: Thunks,
    /// The functions of reference counting, if the module counts.
    pub rt: Option<counts::Runtime>,
    /// The shadow stack, if the module has `#[abi(flat)]` externs or
    /// objects that go on it.
    pub stack: Option<ShadowStack>,
    /// The `let`s whose objects go on the shadow stack, by address.
    pub stack_objects: std::collections::HashSet<*const conduit_ir::Let>,
    /// The free lists of continuations' table slots, when counting.
    pub slot_lists: Option<SlotLists>,
    /// The table of host values, if the module has any.
    pub hosts: Option<Hosts>,
    /// What `host_function` needs, if the module has any.
    pub callbacks: Option<Callbacks>,
    /// Whether an exception from the host can unwind some of its code:
    /// some function has `throw` in its row. Otherwise an entry point
    /// needn't catch one, since one that unwinds the module anyway is
    /// undefined behavior (see "Exceptions from the host" in
    /// `docs/ir.md`).
    pub throws: bool,
    /// Where the host's exceptions come from, if `throw` has a wasm link:
    /// the tag the module imports, number 0, which a handler for `throw`
    /// catches and a `raise` that nothing handles throws (see
    /// `docs/exceptions.md`).
    pub host_tag: Option<(&'m str, &'m str)>,
    /// The tag an aborting clause throws, holding its handler's frame:
    /// the first the module defines, after the host's if it imports it.
    pub abort_tag: u32,
}

/// Closures the host calls, made by `host_function`: in Wasm, a JavaScript
/// function that the runtime makes from a trampoline and the closure's
/// address, with the import `conduit.function(funcref, i32) -> (ref
/// extern)`. Each signature of closure has a trampoline, `(closure,
/// params..) -> results`, which runs the closure as an export runs its
/// function, and the module exports `conduit_release(closure)`, which the
/// runtime calls once the host lets go of the function, to drop the
/// closure.
#[derive(Debug, Clone)]
pub(crate) struct Callbacks {
    /// The function index of the import.
    pub import: u32,
    /// The parameters and results of each signature, in the order of their
    /// trampolines.
    pub sigs: Vec<(Vec<Ty>, Vec<Ty>)>,
    /// The index of the first trampoline; `conduit_release` comes after the
    /// last.
    pub first: u32,
}

impl Callbacks {
    /// The trampoline for closures that take `params` and give `results`.
    pub fn trampoline(&self, params: &[Ty], results: &[Ty]) -> Option<u32> {
        (self.sigs.iter())
            .position(|(p, r)| p == params && r == results)
            .map(|k| self.first + k as u32)
    }

    pub fn release(&self) -> u32 {
        self.first + self.sigs.len() as u32
    }

    /// How many functions they take.
    pub fn len(&self) -> u32 {
        self.sigs.len() as u32 + 1
    }
}

/// The signatures of the closures that `host_function`s in `m` take.
fn callback_sigs(m: &Module) -> Vec<(Vec<Ty>, Vec<Ty>)> {
    let mut sigs = Vec::new();
    for f in m.funcs.values() {
        for l in f.body.all_lets() {
            if l.op == conduit_ir::Op::HostFunction
                && let Some(conduit_ir::Atom::Var(c)) = l.args.first()
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

/// Host values, `Ty::Host`: each is a box, an object of 16 bytes holding
/// the number of its slot in a table of `externref`s at 8, which only an
/// extern's arguments and results see. So a host value is an `i32` like
/// any other object, which fields, closures and `any`s hold as they are,
/// and which is counted as an object. A box whose count gets to 0 clears
/// its slot, so the host can collect what it held, and goes on a free list,
/// linked at 4, which the next host value takes from, keeping its slot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Hosts {
    pub table: u32,
    /// The global holding the free list, when counting.
    pub free: Option<u32>,
}

/// A function the host calls, an export's wrapper or a trampoline: its
/// type and body. It takes `lead` `i32`s as they are, like a trampoline's
/// closure, then `params`, as the host gives them, and gives `results` as
/// the host takes them (see `from_host` and `to_host`). It
/// empties the handler stack, if the module has handlers, and puts it back
/// after, also when an exception unwinds it, if the module
/// [throws](Cx::throws), as it then does with the shadow stack. Each host value it's given goes in a box, and each box it gives
/// back becomes its `externref`, and the box is dropped. `call` makes the
/// call, from the locals that hold the arguments, as Conduit has them.
fn host_entry(
    cx: &mut Cx<'_>,
    lead: u32,
    params: &[Ty],
    results: &[Ty],
    call: impl FnOnce(&[u32]) -> Vec<I<'static>>,
) -> Result<(u32, func::Body), String> {
    let n = lead + params.len() as u32;
    let sig_params = std::iter::repeat_n(ValType::I32, lead as usize);
    let ty = cx.sig(FnSig::new(
        sig_params.chain(params.iter().map(from_host)),
        results.iter().map(to_host),
    ));
    let mut locals: Vec<ValType> = Vec::new();
    let mut local = |t: ValType| {
        locals.push(t);
        n + locals.len() as u32 - 1
    };
    let saved = (!cx.m.handlers.is_empty()).then(|| (local(ValType::I32), local(ValType::I32)));
    let mut code = Vec::new();
    if let Some((handlers, site)) = saved {
        code.extend([
            I::GlobalGet(HANDLERS),
            I::LocalSet(handlers),
            I::GlobalGet(SITE),
            I::LocalSet(site),
            I::I32Const(0),
            I::GlobalSet(HANDLERS),
        ]);
    }
    let stack = cx
        .stack
        .map(|stack| (stack, local(ValType::I32), local(ValType::I32)));
    if let Some((stack, sp, limit)) = stack {
        code.extend(shadow::save(stack, sp, limit));
    }
    // What the caller had. A call that returns leaves the shadow stack as
    // it found it, but one that an exception unwinds may not: see
    // `docs/internals/wasm-unwinding.md`.
    let restore = |unwound: bool| {
        let mut code = Vec::new();
        if let Some((handlers, site)) = saved {
            code.extend([
                I::LocalGet(handlers),
                I::GlobalSet(HANDLERS),
                I::LocalGet(site),
                I::GlobalSet(SITE),
            ]);
        }
        if let Some((stack, sp, limit)) = stack.filter(|_| unwound) {
            code.extend(shadow::restore(stack, sp, limit));
        }
        code
    };
    // The arguments, host ones in boxes. A null is the address 0.
    let mut args = Vec::with_capacity(params.len());
    for (i, t) in params.iter().enumerate() {
        let i = lead + i as u32;
        if !t.is_host() {
            args.push(i);
            continue;
        }
        let h = cx.hosts.ok_or("no table of host values")?;
        let p = local(ValType::I32);
        let boxed = func::box_host(cx.alloc, h, i, p);
        if *t == Ty::NullableHost {
            code.extend([
                I::LocalGet(i),
                I::RefIsNull,
                I::If(wasm_encoder::BlockType::Empty),
                I::I32Const(0),
                I::LocalSet(p),
                I::Else,
            ]);
            code.extend(boxed);
            code.push(I::End);
        } else {
            code.extend(boxed);
        }
        args.push(p);
    }
    if !cx.throws || (saved.is_none() && stack.is_none()) {
        code.extend(call(&args));
    } else {
        // The call, which puts things back as they were however it ends:
        // an exception that unwinds through it, like the host's, is
        // caught, and goes on once they're back.
        let held: Vec<u32> = results.iter().map(|t| local(val(t))).collect();
        let exn = ValType::Ref(RefType::EXNREF);
        let catches = vec![wasm_encoder::Catch::AllRef { label: 0 }];
        code.extend([
            I::Block(wasm_encoder::BlockType::Empty),
            I::Block(wasm_encoder::BlockType::Result(exn)),
            I::TryTable(wasm_encoder::BlockType::Empty, catches.into()),
        ]);
        code.extend(call(&args));
        code.extend(held.iter().rev().map(|&l| I::LocalSet(l)));
        code.extend([I::End, I::Br(1), I::End]);
        code.extend(restore(true));
        code.extend([I::ThrowRef, I::End]);
        code.extend(held.iter().map(|&l| I::LocalGet(l)));
    }
    code.extend(restore(false));
    // The results, host ones as their `externref`s, whose boxes are
    // dropped.
    if results.iter().any(Ty::is_host) {
        let h = cx.hosts.ok_or("no table of host values")?;
        let held: Vec<u32> = results.iter().map(|t| local(val(t))).collect();
        code.extend(held.iter().rev().map(|&l| I::LocalSet(l)));
        let drop = cx
            .rt
            .as_ref()
            .map(|rt| rt.drop(&counts::DropKey::Host, true));
        for (t, &l) in results.iter().zip(&held) {
            code.push(I::LocalGet(l));
            if !t.is_host() {
                continue;
            }
            code.extend([
                I::If(wasm_encoder::BlockType::Result(ValType::EXTERNREF)),
                I::LocalGet(l),
                func::load(Storage::I32, 8),
                I::TableGet(h.table),
                I::Else,
                I::RefNull(wasm_encoder::HeapType::EXTERN),
                I::End,
            ]);
            if let Some(drop) = drop {
                code.extend([I::LocalGet(l), I::Call(drop)]);
            }
        }
    }
    code.push(I::End);
    Ok((ty, func::Body { locals, code }))
}

/// Whether function `f` takes or gives a host value.
fn takes_or_gives_host(m: &Module, f: FuncId) -> bool {
    m.funcs.get(&f).is_some_and(|func| {
        (func.params.iter().map(|v| func.var_ty(*v)))
            .chain(&func.results)
            .any(Ty::is_host)
    })
}

/// What a value of type `ty` is where the host gives it, as an export's
/// parameter or an import's result: a host value is the host's own
/// reference, a `(ref extern)` for one that's never null, and an
/// `externref` for a nullable one. So the host can't give a `null` where
/// one isn't expected: a JavaScript function that gives one there throws
/// at the call. The JS String Builtins that give a string have that type.
fn from_host(ty: &Ty) -> ValType {
    match ty {
        Ty::Host => ValType::Ref(RefType {
            nullable: false,
            heap_type: wasm_encoder::HeapType::EXTERN,
        }),
        Ty::NullableHost => ValType::EXTERNREF,
        ty => val(ty),
    }
}

/// What a value of type `ty` is where the host takes it, as an export's
/// result or an import's parameter: an `externref` for a host value of
/// either kind, which the JS String Builtins want. Everywhere else, a host
/// value is its slot's box.
fn to_host(ty: &Ty) -> ValType {
    match ty {
        Ty::Host | Ty::NullableHost => ValType::EXTERNREF,
        ty => val(ty),
    }
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
    /// With counts, for each operation: cancels its continuation.
    pub cancel: Vec<u32>,
    pub kont_slots: Vec<u32>,
    pub disc_slots: Vec<u32>,
    pub conts: Vec<u32>,
    kont_funcs: Vec<u32>,
    disc_funcs: Vec<u32>,
    /// With counts, for each operation: the functions that free its
    /// Conduit continuations and step through what they hold.
    kont_frees: Vec<u32>,
    kont_children: Vec<u32>,
}

/// With counts and handlers that switch stacks, the free lists of table
/// slots: the global holding each continuation table's list, by table
/// minus 1, and the one holding the spare nodes. A node is 8 bytes: the
/// next node, then the slot.
#[derive(Debug, Clone)]
pub(crate) struct SlotLists {
    pub heads: Vec<u32>,
    pub spare: u32,
}

/// How [`emit_module_with`] lowers a module.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Whether to lower handlers with a clause that's neither
    /// tail-resumptive nor aborting with the stack switching proposal,
    /// which isn't standard yet and only some engines run, such as
    /// Wasmtime on x86-64 Linux and macOS. Otherwise, a module with such a
    /// handler is lowered by evidence passing, which runs anywhere but
    /// costs more.
    pub stack_switching: bool,
    /// Whether the module's reference counts are explicit, as
    /// `conduit_opt::ownership::insert_counts` makes them, so that memory
    /// is freed: `dup`, `drop` and the ops that count for themselves count,
    /// and an `any` is always a box. Otherwise those ops do nothing, and
    /// memory is never freed. Effects must be lowered before counts are
    /// inserted, if they're lowered by evidence passing; with
    /// `stack_switching`, they needn't be. The module then exports the
    /// global `conduit_live`, the number of objects allocated and not yet
    /// freed, handler frames and continuations among them.
    pub counts: bool,
    /// With `counts`, whether to collect cycles too, by trial deletion
    /// (see `counts.rs`). The module then exports `conduit_collect`, which
    /// collects what's buffered so far.
    pub cycles: bool,
    /// The bytes of the main stack's first segment of the shadow stack,
    /// which holds what an `#[abi(flat)]` extern call passes through
    /// memory, out-parameters and iovec lists, and objects that don't
    /// outlive their function. A module with neither has no shadow stack.
    pub shadow_stack: u32,
    /// The bytes of a segment of the shadow stack, which each
    /// continuation starts on with `stack_switching`, and which a push
    /// that doesn't fit chains another one of, or a bigger one for what
    /// doesn't fit in one. Segments go back on a free list when what's on
    /// them is popped, and a continuation's when its body returns or
    /// throws, or when it's freed unused.
    pub continuation_stack: u32,
    /// Whether objects that never outlive the function that makes them go
    /// on the shadow stack instead of the heap (see `escape.rs`).
    pub stack_objects: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            stack_switching: false,
            counts: false,
            cycles: false,
            shadow_stack: 64 * 1024,
            continuation_stack: 4 * 1024,
            stack_objects: true,
        }
    }
}

/// The shadow stack: memory that grows down, in segments. The main stack
/// starts on a region of its own, from [`base`](Self::base) to
/// [`top`](Self::top), and each continuation on a segment of
/// [`segment`](Self::segment) bytes. A push that doesn't fit in the
/// current segment chains a new one, from the free list or the heap.
///
/// A segment starts with a header of [`HEADER`] bytes: the pointer and
/// limit to go back to when it's popped, and its size. A continuation's
/// first segment holds the continuation's own pointer and limit there
/// instead while it's suspended, and a free segment the next free one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ShadowStack {
    /// The globals: the lowest address in use, the lowest the current
    /// segment has, and the first free segment, or 0.
    pub sp: u32,
    pub limit: u32,
    pub free: u32,
    pub base: u32,
    pub top: u32,
    /// The bytes of a segment, its header's included.
    pub segment: u32,
    /// Whether each continuation has a segment of its own: whether some
    /// handler switches stacks.
    pub continuations: bool,
    /// `grow(n)`, which chains a segment that `n` more bytes fit in, and
    /// `release(from, to)`, which gives back the segments from the one
    /// whose limit is `from` until the one whose limit is `to`.
    pub grow: u32,
    pub release: u32,
}

/// The bytes of a segment's header.
pub(crate) const HEADER: u32 = 16;

impl Cx<'_> {
    /// The index of the first function after `alloc` and reference
    /// counting's.
    pub fn after_alloc(&self) -> u32 {
        self.shadow_funcs()
            + u32::from(self.stack.is_some()) * 2
            + self.callbacks.as_ref().map_or(0, Callbacks::len)
    }

    /// The index of the shadow stack's functions, if there is one: after
    /// reference counting's.
    pub fn shadow_funcs(&self) -> u32 {
        self.alloc + 1 + self.rt.as_ref().map_or(0, |rt| rt.len())
    }

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

    /// The number of a handler, which its frames hold.
    pub fn handler_number(&self, h: HandlerId) -> Result<i32, String> {
        self.m
            .handlers
            .keys()
            .position(|&k| k == h)
            .map(|i| i as i32 + 1)
            .ok_or_else(|| format!("unknown handler {h}"))
    }

    /// The handlers that handle `e`, by their number, with where each
    /// keeps its clause for `e.i` in its frames.
    pub fn handlers_of(&self, e: EffectId, i: u32) -> Vec<(i32, u32)> {
        (self.m.handlers.values().zip(1..))
            .filter_map(|(decl, n)| Some((n, 16 + 4 * decl.slot(e, i)? as u32)))
            .collect()
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

    /// The host's tag, if the module imports it.
    pub fn host_tag(&self) -> Option<u32> {
        self.host_tag.map(|_| 0)
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
    /// fields are the handler's number, the next frame, a slot per clause,
    /// the environment, and the results if `h` has an aborting clause.
    pub fn frame(&self, h: HandlerId) -> Result<(Vec<(u32, Storage)>, u32), String> {
        let decl = self
            .m
            .handlers
            .get(&h)
            .ok_or_else(|| format!("unknown handler {h}"))?;
        let mut tys = vec![Ty::I32, Ty::I32];
        tys.extend(std::iter::repeat_n(Ty::I32, decl.len()));
        tys.extend(decl.env.iter().cloned());
        if self.aborting.contains(&h) {
            let ret = self
                .m
                .funcs
                .get(&decl.ret)
                .ok_or_else(|| format!("unknown return clause {}", decl.ret))?;
            tys.extend(ret.results.iter().cloned());
        }
        // A handler that switches stacks keeps its continuation's segment
        // last.
        if self.segment_field(h) {
            tys.push(Ty::I32);
        }
        Ok(layout(&tys))
    }

    /// Whether `h`'s frame ends with its continuation's shadow stack
    /// segment.
    pub fn segment_field(&self, h: HandlerId) -> bool {
        self.is_switching(h) && self.stack.is_some_and(|s| s.continuations)
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
        match conduit_ir::const_value(self.m, c) {
            Some(ConstValue::Lit(l)) => Ok(Err(*l)),
            _ => Err(format!("unknown constant {c}")),
        }
    }
}

/// The import's signature of an `#[abi(flat)]` extern: see [`Abi::Flat`].
fn flat_sig(ext: &conduit_ir::Extern) -> FnSig {
    let mut params = Vec::new();
    for (i, ty) in ext.sig.params.iter().enumerate() {
        match ty {
            Ty::Array(..) if ext.addr(i) => params.push(ValType::I32),
            Ty::Array(..) => params.extend([ValType::I32, ValType::I32]),
            ty => params.push(val(ty)),
        }
    }
    params.extend(ext.sig.results.iter().skip(1).map(|_| ValType::I32));
    FnSig {
        params,
        results: ext.sig.results.iter().take(1).map(val).collect(),
    }
}

/// Lowers a module that has passed [`conduit_ir::verify::verify_module`]
/// to a Wasm binary with objects in linear memory, exporting each function
/// with a `#[export(wasm, ..)]` under its name, and the memory as
/// `memory`, with the default [`Options`]: without stack switching. An
/// export of another kind is an error: it's for another target.
///
/// An export starts with no handlers, even when the host calls it from
/// inside an extern: an operation it doesn't handle itself traps, rather
/// than reaching the handlers around that extern's call, behind the
/// host's frame. Effects without operations, which only limit who may
/// call an extern, are never performed, so they never trap.
pub fn emit_module(m: &Module) -> Result<Vec<u8>, Error> {
    emit_module_with(m, &Options::default())
}

/// [`emit_module`] with `options`. A module with a handler whose clauses
/// aren't all tail-resumptive or aborting is lowered with stack switching
/// if the options ask for it, and by evidence passing first otherwise,
/// which keeps every function's ID.
pub fn emit_module_with(m: &Module, options: &Options) -> Result<Vec<u8>, Error> {
    let (lowered, thunks) = lower_handlers(m, options)?;
    let m = lowered.as_ref().unwrap_or(m);
    let exports = wasm_exports(m)?;
    let switching = switching_handlers(m);
    let plain = PlainClauses::new(m, &switching);
    let mut cx = Cx::new(m, options, &switching, &plain, thunks)?;
    let (data, heap_base) = lay_out_memory(&mut cx, options, &exports)?;
    let imports = imports(&mut cx)?;

    // Every function's body, in the order of their indices: the module's
    // functions, `alloc`, the runtime's, the clauses' wrappers, those of
    // handlers that switch stacks, the closures' wrappers, the closures'
    // free functions, and the exports' wrappers.
    let funcs = lower_funcs(&mut cx, &plain.funcs)?;
    let alloc_ty = cx.sig(FnSig::new([ValType::I32], [ValType::I32]));
    let alloc = match &cx.rt {
        Some(rt) => counts::lower_alloc(rt.heads),
        None => func::lower_alloc(),
    };
    let clause_wrappers = lower_clause_wrappers(&mut cx, &switching, &plain.funcs)?;
    let switched = lower_switching(&mut cx, &switching)?;
    // The host's tag's type, which it imports, then the others, after
    // the abort's.
    let host_tag_ty = cx
        .host_tag
        .map(|_| cx.sig(FnSig::new([ValType::EXTERNREF], [])));
    let disc_tag_ty = cx.disc_tag.map(|_| cx.sig(FnSig::new([ValType::I32], [])));
    let cancel_tag_ty = cx.cancel_tag.map(|_| cx.sig(FnSig::new([], [])));
    let mut control_tag_tys = Vec::new();
    for &(e, i) in cx.tags.clone().keys() {
        let op = &m.effects[&e].ops[i as usize];
        control_tag_tys.push(cx.sig(FnSig::new(
            op.params.iter().map(val),
            op.results.iter().map(val),
        )));
    }
    // The tables of Wasm continuations, after the function table.
    let mut cont_tables = Vec::new();
    for &h in &switching {
        for i in 0..m.handlers[&h].len() as u32 {
            cont_tables.push(cx.op_cont(h, i).map_err(Error::at(Item::Handler(h)))?);
        }
    }
    let abort_ty = (!cx.aborts.is_empty()).then(|| cx.sig(FnSig::new([ValType::I32], [])));
    let entries = lower_entries(&mut cx)?;
    let (runtime, closure_frees) = lower_runtime(&mut cx)?;
    let before_wrappers = funcs.len()
        + 1
        + runtime.len()
        + clause_wrappers.len()
        + switched.len()
        + entries.len()
        + closure_frees.len();
    let first = cx.alloc - m.funcs.len() as u32;
    let (export_wrappers, export_index) =
        lower_export_wrappers(&mut cx, &exports, first + before_wrappers as u32)?;
    // `conduit_reset`, after every other function.
    let reset = has_state(&cx).then(|| {
        let index = first + (before_wrappers + export_wrappers.len()) as u32;
        (index, lower_reset(&mut cx))
    });

    let mut functions = FunctionSection::new();
    let mut code = CodeSection::new();
    let bodies = (funcs.iter().map(|(ty, b)| (*ty, b)))
        .chain(std::iter::once((alloc_ty, &alloc)))
        .chain(runtime.iter().map(|(ty, b)| (*ty, b)))
        .chain(clause_wrappers.iter().map(|(ty, b)| (*ty, b)))
        .chain(switched.iter().map(|(ty, b)| (*ty, b)))
        .chain(entries.iter().map(|(ty, b)| (*ty, b)))
        .chain(closure_frees.iter().map(|(ty, b)| (*ty, b)))
        .chain(export_wrappers.iter().map(|(ty, b)| (*ty, b)))
        .chain(reset.iter().map(|(_, (ty, b))| (*ty, b)));
    for (ty, body) in bodies {
        functions.function(ty);
        let mut f = Function::new_with_locals_types(body.locals.iter().copied());
        for i in &body.code {
            f.instruction(i);
        }
        code.function(&f);
    }

    let mut module = wasm_encoder::Module::new();
    // After everything that adds a type, export wrappers among them.
    module.section(&type_section(&cx));
    let mut import_section = ImportSection::new();
    for &(module, name, sig) in &imports {
        import_section.import(module, name, EntityType::Function(sig));
    }
    if let (Some((module, name)), Some(ty)) = (cx.host_tag, host_tag_ty) {
        import_section.import(
            module,
            name,
            EntityType::Tag(TagType {
                kind: TagKind::Exception,
                func_type_idx: ty,
            }),
        );
    }
    module.section(&import_section);
    module.section(&functions);
    module.section(&table_section(&cx, &cont_tables));
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
    // one that cancels a continuation, then the control tags.
    let tag_tys: Vec<u32> = abort_ty
        .into_iter()
        .chain(disc_tag_ty)
        .chain(cancel_tag_ty)
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
    module.section(&global_section(&cx, heap_base));
    let reset_index = reset.as_ref().map(|(index, _)| *index);
    module.section(&export_section(&cx, &exports, &export_index, reset_index)?);
    let elements = element_section(&cx);
    if !elements.is_empty() {
        module.section(&elements);
    }
    module.section(&code);
    if !data.is_empty() {
        let mut segments = DataSection::new();
        segments.active(0, &ConstExpr::i32_const(CONST_BASE as i32), data);
        module.section(&segments);
    }
    if let Some(names) = name_section(m, &cx, &export_index) {
        module.section(&names);
    }
    Ok(module.finish())
}

/// A body of a function, with the index of its type.
type Lowered = (u32, func::Body);

/// The function `discontinue E.i` performs with, by operation.
type Thunks = BTreeMap<(EffectId, u32), FuncId>;

/// `m`, or a copy with its handlers lowered as `options` asks: a module
/// with a handler whose clauses aren't all tail-resumptive or aborting is
/// lowered by evidence passing, unless it switches stacks, which needs a
/// function for each operation that's discontinued with (see
/// [`with_thunks`]).
fn lower_handlers(m: &Module, options: &Options) -> Result<(Option<Module>, Thunks), Error> {
    if conduit_ir::clause::direct_handlers(m) {
        return Ok((None, BTreeMap::new()));
    }
    let err = Error::at(Item::Func(FuncId(0)));
    if options.stack_switching {
        let (copy, thunks) = with_thunks(m);
        return Ok((Some(copy), thunks));
    }
    if options.counts {
        return Err(err(
            "a module that counts references and has handlers that need evidence \
             passing must have its effects lowered before its counts are inserted"
                .into(),
        ));
    }
    let mut copy = m.clone();
    conduit_effects::lower_effects(&mut copy).map_err(|e| err(format!("lowering effects: {e}")))?;
    Ok((Some(copy), BTreeMap::new()))
}

/// The functions `m` exports, by the names it exports them as. Each export
/// must be a Wasm one.
fn wasm_exports(m: &Module) -> Result<Vec<(FuncId, &str)>, Error> {
    let mut exports = Vec::new();
    for (&f, export) in &m.exports {
        let Export::Wasm { name } = export else {
            return Err(Error::at(Item::Func(f))(format!(
                "can't export a function as {} `{}`: only with a `#[export(wasm, ..)]`",
                export.kind(),
                export.name()
            )));
        };
        exports.push((f, &**name));
    }
    Ok(exports)
}

/// The handlers of `m` that switch stacks: those with a clause that's
/// neither tail-resumptive nor aborting. A clause is lowered one way, so a
/// handler that shares one with a handler that switches stacks switches
/// too.
fn switching_handlers(m: &Module) -> BTreeSet<HandlerId> {
    let mut switching: BTreeSet<HandlerId> = (m.handlers.keys().copied())
        .filter(|&h| !conduit_ir::clause::direct_handler(m, h))
        .collect();
    loop {
        let shared: BTreeSet<FuncId> = (switching.iter())
            .flat_map(|h| m.handlers[h].clauses().map(|(_, _, c)| c))
            .collect();
        let more: Vec<HandlerId> = (m.handlers.iter())
            .filter(|(h, d)| {
                !switching.contains(h) && d.clauses().any(|(_, _, c)| shared.contains(&c))
            })
            .map(|(&h, _)| h)
            .collect();
        if more.is_empty() {
            return switching;
        }
        switching.extend(more);
    }
}

/// The clauses of handlers that don't switch stacks, which are lowered as
/// plain functions, without their continuation: tail-resumptive or
/// aborting.
struct PlainClauses {
    funcs: BTreeMap<FuncId, Func>,
    /// The aborting clauses, and their handlers.
    aborts: BTreeSet<FuncId>,
    aborting: BTreeSet<HandlerId>,
    /// Whether one of them discontinues.
    discontinues: bool,
}

impl PlainClauses {
    fn new(m: &Module, switching: &BTreeSet<HandlerId>) -> PlainClauses {
        let mut plain = PlainClauses {
            funcs: BTreeMap::new(),
            aborts: BTreeSet::new(),
            aborting: BTreeSet::new(),
            discontinues: false,
        };
        for (&h, decl) in &m.handlers {
            if switching.contains(&h) {
                continue;
            }
            for (_, _, clause) in decl.clauses() {
                let Some(f) = m.funcs.get(&clause) else {
                    continue;
                };
                if let Some(f) = conduit_ir::clause::plain_call(f) {
                    plain.discontinues |= conduit_ir::clause::discontinues(&f);
                    plain.funcs.insert(clause, f);
                } else if let Some(f) = conduit_ir::clause::aborting(f) {
                    plain.funcs.insert(clause, f);
                    plain.aborts.insert(clause);
                    plain.aborting.insert(h);
                }
            }
        }
        plain
    }
}

impl<'m> Cx<'m> {
    /// The context for lowering `m`, with every function's index known:
    /// those of the module, after the externs and `host_function`'s
    /// import, then `alloc`, then those of reference counting, the shadow
    /// stack, `host_function`'s trampolines and handlers that switch
    /// stacks.
    fn new(
        m: &'m Module,
        options: &Options,
        switching: &BTreeSet<HandlerId>,
        plain: &PlainClauses,
        thunks: Thunks,
    ) -> Result<Cx<'m>, Error> {
        let host_tag = match m
            .effects
            .get(&EffectId::THROW)
            .and_then(|e| e.link.as_ref())
        {
            None => None,
            Some(Link::Wasm { module, name }) => Some((&**module, &**name)),
            Some(other) => {
                return Err(Error::at(Item::Effect(EffectId::THROW))(format!(
                    "throw has a {} link, for another target: only a `#[link(wasm, ..)]` \
                     names a tag to import",
                    other.kind()
                )));
            }
        };
        // The clauses' wrappers come first in the table.
        let mut wrappers = BTreeMap::new();
        let mut wrapper_slots = 0;
        for (&h, decl) in &m.handlers {
            if !switching.contains(&h) {
                wrappers.insert(h, wrapper_slots);
                wrapper_slots += decl.len() as u32;
            }
        }
        // The import `host_function` needs comes after the externs.
        let callback_sigs = callback_sigs(m);
        let first = m.externs.len() as u32 + u32::from(!callback_sigs.is_empty());
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
            clause_slots: wrapper_slots,
            aborting: plain.aborting.clone(),
            aborts: plain.aborts.clone(),
            discontinues: plain.discontinues,
            switches: BTreeMap::new(),
            switch_funcs: 0,
            tags: BTreeMap::new(),
            disc_tag: None,
            cancel_tag: None,
            thunks,
            rt: None,
            stack: None,
            stack_objects: Default::default(),
            slot_lists: None,
            hosts: None,
            callbacks: None,
            throws: m.funcs.values().any(|f| f.row.throws()),
            host_tag,
            abort_tag: u32::from(host_tag.is_some()),
        };
        if options.counts {
            cx.rt = Some(counts::Runtime::new(m, cx.alloc + 1, options.cycles));
        }
        if options.stack_objects {
            cx.stack_objects = escape::stack_objects(m);
        }
        // A shadow stack, where it's laid out later, but whose functions need
        // their indices now. Its globals come after the others.
        if m.externs.values().any(|e| e.abi == Abi::Flat) || !cx.stack_objects.is_empty() {
            let sp = if cx.rt.is_some() { 9 } else { 3 };
            let grow = cx.shadow_funcs();
            cx.stack = Some(ShadowStack {
                sp,
                limit: sp + 1,
                free: sp + 2,
                base: 0,
                top: 0,
                segment: options
                    .continuation_stack
                    .max(2 * HEADER)
                    .next_multiple_of(8),
                continuations: !switching.is_empty(),
                grow,
                release: grow + 1,
            });
        }
        // The trampolines and `conduit_release`, after the shadow stack's
        // functions.
        if !callback_sigs.is_empty() {
            cx.callbacks = Some(Callbacks {
                import: m.externs.len() as u32,
                sigs: callback_sigs,
                first: cx.shadow_funcs() + u32::from(cx.stack.is_some()) * 2,
            });
        }
        if let Some(&h) = switching.first() {
            cx.switch_funcs =
                plan_switching(&mut cx, switching).map_err(Error::at(Item::Handler(h)))?;
        }
        Ok(cx)
    }
}

/// Lays out memory: the constants from [`CONST_BASE`], then reference
/// counting's free lists, then the shadow stack, with the globals and
/// tables that go with them. Gives the bytes of the constants, from
/// [`CONST_BASE`], and where the heap starts.
fn lay_out_memory(
    cx: &mut Cx<'_>,
    options: &Options,
    exports: &[(FuncId, &str)],
) -> Result<(Vec<u8>, u32), Error> {
    let m = cx.m;
    let mut data = vec![0u8; CONST_BASE as usize];
    for &c in m.consts.keys() {
        if let Some(addr) = lay_out_const(cx, &mut data, c).map_err(Error::at(Item::Const(c)))? {
            cx.consts.insert(c, addr);
        }
    }
    let mut heap_base = (data.len() as u32).next_multiple_of(8);
    // The heads of the free lists, after the constants.
    if let Some(rt) = &mut cx.rt {
        rt.heads = heap_base;
        heap_base += rt.memory();
    }
    // The shadow stack, after them, and its pointer after the other globals.
    if let Some(stack) = &mut cx.stack {
        stack.base = heap_base;
        heap_base += options.shadow_stack.next_multiple_of(8);
        stack.top = heap_base;
    }
    // The free lists of slots, after the shadow stack's globals.
    if cx.rt.is_some() && !cx.switches.is_empty() {
        let next = match cx.stack {
            Some(stack) => stack.free + 1,
            None => 9,
        };
        let tables: usize = cx.switches.values().map(|sw| sw.conts.len()).sum();
        cx.slot_lists = Some(SlotLists {
            spare: next,
            heads: (next + 1..next + 1 + tables as u32).collect(),
        });
    }
    // The table of host values, after the continuations', and its free
    // list after the other globals.
    let uses_hosts = m
        .externs
        .values()
        .any(|e| e.sig.params.iter().chain(&e.sig.results).any(Ty::is_host))
        || cx
            .rt
            .as_ref()
            .is_some_and(|rt| rt.keys.contains(&counts::DropKey::Host))
        || exports.iter().any(|&(f, _)| takes_or_gives_host(m, f))
        || cx.callbacks.is_some()
        || cx.host_tag.is_some();
    if uses_hosts {
        let conts: usize = cx.switches.values().map(|sw| sw.conts.len()).sum();
        let free = cx.rt.is_some().then(|| match (&cx.slot_lists, cx.stack) {
            (Some(lists), _) => lists.spare + 1 + lists.heads.len() as u32,
            (None, Some(stack)) => stack.free + 1,
            (None, None) => 9,
        });
        cx.hosts = Some(Hosts {
            table: 1 + conts as u32,
            free,
        });
    }
    Ok((data[CONST_BASE as usize..].to_vec(), heap_base))
}

/// The imports: each extern, which must be a Wasm import, then
/// `host_function`'s, if the module has one. Each is its module, its name
/// and its type.
fn imports<'m>(cx: &mut Cx<'m>) -> Result<Vec<(&'m str, &'m str, u32)>, Error> {
    let mut imports = Vec::new();
    for (&id, ext) in &cx.m.externs {
        // Only a Wasm import can be one: a module for another target has
        // other links, which the frontend chose.
        let Link::Wasm { module, name } = &ext.link else {
            return Err(Error::at(Item::Extern(id))(format!(
                "can't import a {} extern: only one with a `#[link(wasm, ..)]`",
                ext.link.kind()
            )));
        };
        let sig = match ext.abi {
            Abi::Conduit => FnSig::new(
                ext.sig.params.iter().map(to_host),
                ext.sig.results.iter().map(from_host),
            ),
            Abi::Flat => flat_sig(ext),
        };
        imports.push((&**module, &**name, cx.sig(sig)));
    }
    if cx.callbacks.is_some() {
        let sig = cx.sig(FnSig::new(
            [ValType::FUNCREF, ValType::I32],
            [from_host(&Ty::Host)],
        ));
        imports.push(("conduit", "function", sig));
    }
    Ok(imports)
}

/// The module's functions, a plain clause in place of the clause it's of.
fn lower_funcs<'m>(
    cx: &mut Cx<'m>,
    plain: &'m BTreeMap<FuncId, Func>,
) -> Result<Vec<Lowered>, Error> {
    let mut funcs = Vec::new();
    for (&id, f) in &cx.m.funcs {
        let f = plain.get(&id).unwrap_or(f);
        let ty = cx.sig(FnSig::new(
            f.params.iter().map(|&p| val(f.var_ty(p))),
            f.results.iter().map(val),
        ));
        let body = func::lower_func(cx, f).map_err(Error::at(Item::Func(id)))?;
        funcs.push((ty, body));
    }
    Ok(funcs)
}

/// The wrappers of clauses, which `perform` calls through the table.
fn lower_clause_wrappers<'m>(
    cx: &mut Cx<'m>,
    switching: &BTreeSet<HandlerId>,
    plain: &'m BTreeMap<FuncId, Func>,
) -> Result<Vec<Lowered>, Error> {
    let mut wrappers = Vec::new();
    for (&h, decl) in &cx.m.handlers {
        if switching.contains(&h) {
            continue;
        }
        let err = Error::at(Item::Handler(h));
        for i in 0..decl.len() as u32 {
            let (e, j, clause) = clause_at(decl, i);
            let ty = cx.clause_sig(e, j).map_err(&err)?;
            let plain = plain
                .get(&clause)
                .ok_or_else(|| err(format!("clause {clause} isn't tail-resumptive or aborting")))?;
            let body = func::lower_wrapper(cx, h, i, plain).map_err(&err)?;
            wrappers.push((ty, body));
        }
    }
    Ok(wrappers)
}

/// The functions of handlers that switch stacks, in the order
/// `plan_switching` numbered them.
fn lower_switching(
    cx: &mut Cx<'_>,
    switching: &BTreeSet<HandlerId>,
) -> Result<Vec<Lowered>, Error> {
    let m = cx.m;
    let mut switched = Vec::new();
    for &h in switching {
        let err = Error::at(Item::Handler(h));
        let n = m.handlers[&h].len() as u32;
        let disc = cx.disc_tag.is_some();
        let sig = cx.drive_sig(h, switch::Start::Body).map_err(&err)?;
        let body = switch::lower_drive(cx, h, switch::Start::Body).map_err(&err)?;
        switched.push((cx.sig(sig), body));
        let tramp_sig = FnSig::new([ValType::I32], cx.body_results(h).map_err(&err)?);
        let body = switch::lower_tramp(cx, h).map_err(&err)?;
        switched.push((cx.sig(tramp_sig), body));
        for i in 0..n {
            let start = switch::Start::Resume(i);
            let sig = cx.drive_sig(h, start).map_err(&err)?;
            let body = switch::lower_drive(cx, h, start).map_err(&err)?;
            switched.push((cx.sig(sig), body));
        }
        let results = cx.handle_results(h).map_err(&err)?;
        for i in 0..n {
            let op = op_at(m, &m.handlers[&h], i);
            let ksig = conduit_ir::Sig::new(
                op.results.clone(),
                m.funcs[&m.handlers[&h].ret].results.clone(),
            );
            let ty = cx.entry_sig(&ksig);
            let body = switch::lower_kont(cx, h, i, false).map_err(&err)?;
            switched.push((ty, body));
        }
        if disc {
            for i in 0..n {
                let start = switch::Start::Throw(i);
                let sig = cx.drive_sig(h, start).map_err(&err)?;
                let body = switch::lower_drive(cx, h, start).map_err(&err)?;
                switched.push((cx.sig(sig), body));
            }
            for i in 0..n {
                let ty = cx.disc_sig(results.clone());
                let body = switch::lower_kont(cx, h, i, true).map_err(&err)?;
                switched.push((ty, body));
            }
        }
        if cx.rt.is_some() {
            let one = cx.sig(FnSig::new([ValType::I32], []));
            let two = cx.sig(FnSig::new([ValType::I32, ValType::I32], []));
            for i in 0..n {
                let body = switch::lower_kont_free(cx, h, i).map_err(&err)?;
                switched.push((one, body));
            }
            for i in 0..n {
                let body = switch::lower_kont_children(cx, h, i).map_err(&err)?;
                switched.push((two, body));
            }
            for i in 0..n {
                let start = switch::Start::Cancel(i);
                let sig = cx.drive_sig(h, start).map_err(&err)?;
                let body = switch::lower_drive(cx, h, start).map_err(&err)?;
                switched.push((cx.sig(sig), body));
            }
        }
    }
    Ok(switched)
}

/// The wrappers of closures, as many as lowering asked for, which lowering
/// them can ask for more of.
fn lower_entries(cx: &mut Cx<'_>) -> Result<Vec<Lowered>, Error> {
    let mut entries = Vec::new();
    let mut k = 0;
    while k < cx.entries.len() {
        let (f, n) = cx.entries[k];
        let sig = cx.m.funcs[&f].sig();
        let rest = conduit_ir::Sig::new(sig.params[n..].to_vec(), sig.results.clone());
        let ty = cx.entry_sig(&rest);
        let body = func::lower_entry(cx, f, n).map_err(Error::at(Item::Func(f)))?;
        entries.push((ty, body));
        k += 1;
    }
    Ok(entries)
}

/// The runtime's functions, which go after `alloc`: reference counting's,
/// the shadow stack's, then `host_function`'s trampolines and
/// `conduit_release`. And, with counts, the closures' free functions and
/// children, which go after the closures' wrappers.
fn lower_runtime(cx: &mut Cx<'_>) -> Result<(Vec<Lowered>, Vec<Lowered>), Error> {
    // Reference counting's functions, now that the closures are known: a
    // closure's free function is at its wrapper's slot plus the number of
    // wrappers, and a kind's drop after those.
    let n_closures = cx.entries.len() as u32;
    let mut runtime = Vec::new();
    let mut closure_frees = Vec::new();
    // The table, after the closures' wrappers: their free functions, the
    // kinds' drops, the closures' children, the kinds' children, and the
    // kinds' steps.
    if let Some(rt) = cx.rt.take() {
        let (w, e, k) = (
            cx.wrapper_slots as i32,
            n_closures as i32,
            rt.keys.len() as i32,
        );
        // After the kinds' steps, a continuation's free function for each
        // continuation slot, then its children.
        let end = w + 3 * e + 3 * k;
        let (first, konts) = (cx.clause_slots as i32, w - cx.clause_slots as i32);
        let slots = counts::Slots {
            closure_free: e,
            kont_free: end - first,
            kont_children: end + konts - first,
            kind_drop: w + 2 * e,
            closure_children: 2 * e + k,
            kind_children: w + 3 * e + k,
            kind_step: w + 3 * e + 2 * k,
        };
        runtime = rt.bodies(cx, slots);
        let one = cx.sig(FnSig::new([ValType::I32], []));
        let two = cx.sig(FnSig::new([ValType::I32, ValType::I32], []));
        for &(f, n) in &cx.entries {
            closure_frees.push((one, rt.closure_free(cx, f, n)));
        }
        for &(f, n) in &cx.entries {
            closure_frees.push((two, rt.closure_children(cx, f, n)));
        }
        cx.rt = Some(rt);
    }
    // The shadow stack's functions, after reference counting's.
    if let Some(stack) = cx.stack {
        runtime.extend(shadow::bodies(cx, stack));
    }
    // The trampolines and `conduit_release`, after them.
    if let Some(cb) = cx.callbacks.clone() {
        for (params, results) in &cb.sigs {
            let sig = conduit_ir::Sig::new(params.clone(), results.clone());
            let entry = cx.entry_sig(&sig);
            let dup = cx.rt.as_ref().map(|rt| rt.dup());
            let body = host_entry(cx, 1, params, results, |args| {
                // The host keeps the closure, and `apply` takes one.
                let mut code = Vec::new();
                if let Some(dup) = dup {
                    code.extend([I::LocalGet(0), I::Call(dup)]);
                }
                code.push(I::LocalGet(0));
                code.extend(args.iter().map(|&a| I::LocalGet(a)));
                code.extend([
                    I::LocalGet(0),
                    func::load(Storage::I32, 4),
                    I::CallIndirect {
                        type_index: entry,
                        table_index: 0,
                    },
                ]);
                code
            })
            .map_err(Error::at(Item::Func(FuncId(0))))?;
            runtime.push(body);
        }
        let one = cx.sig(FnSig::new([ValType::I32], []));
        let drop = cx
            .rt
            .as_ref()
            .map(|rt| rt.drop(&counts::DropKey::Closure, false));
        let mut code = Vec::new();
        if let Some(drop) = drop {
            code.extend([I::LocalGet(0), I::Call(drop)]);
        }
        code.push(I::End);
        runtime.push((
            one,
            func::Body {
                locals: Vec::new(),
                code,
            },
        ));
    }
    Ok((runtime, closure_frees))
}

/// The wrappers of exports, the first at index `next`, with the index of
/// each. With handler frames in memory, each export runs under a wrapper
/// that empties the handler stack and puts it back after, and with a
/// shadow stack, in a module that [throws](Cx::throws), under one that
/// puts its pointer back, however the export ends. An export that takes or gives host values runs under one too,
/// which converts them (see `host_entry`).
fn lower_export_wrappers(
    cx: &mut Cx<'_>,
    exports: &[(FuncId, &str)],
    mut next: u32,
) -> Result<(Vec<Lowered>, BTreeMap<FuncId, u32>), Error> {
    let m = cx.m;
    let mut wrappers = Vec::new();
    let mut index = BTreeMap::new();
    for &(f, _) in exports {
        if index.contains_key(&f) {
            continue;
        }
        let Some(func) = m.funcs.get(&f) else {
            continue;
        };
        // The shadow stack only needs putting back if an exception can
        // unwind the export.
        let stack = cx.stack.is_some() && cx.throws;
        if m.handlers.is_empty() && !stack && !takes_or_gives_host(m, f) {
            continue;
        }
        let err = Error::at(Item::Func(f));
        let params = func.sig().params;
        let idx = cx.func_index(f).map_err(&err)?;
        let wrapper = host_entry(cx, 0, &params, &func.results, |args| {
            let mut code: Vec<I<'static>> = args.iter().map(|&a| I::LocalGet(a)).collect();
            code.push(I::Call(idx));
            code
        })
        .map_err(&err)?;
        wrappers.push(wrapper);
        index.insert(f, next);
        next += 1;
    }
    Ok((wrappers, index))
}

/// Whether the module has state that code unwinding through it can leave
/// behind: the handler stack, or the shadow stack.
fn has_state(cx: &Cx<'_>) -> bool {
    !cx.m.handlers.is_empty() || cx.stack.is_some()
}

/// `conduit_reset()`, which puts the handler stack and the shadow stack
/// back as they are when the module starts, and gives back the shadow
/// stack's segments chained since. The host calls it when nothing of the
/// module's is running, after a trap, which no Wasm code can catch, ended
/// a call: see `docs/internals/wasm-unwinding.md`.
fn lower_reset(cx: &mut Cx<'_>) -> Lowered {
    let ty = cx.sig(FnSig::new([], []));
    let mut code = vec![
        I::I32Const(0),
        I::GlobalSet(HANDLERS),
        I::I32Const(0),
        I::GlobalSet(SITE),
    ];
    if let Some(stack) = cx.stack {
        // The segments chained on the main stack lead back to it. With
        // continuations, the stack may be a continuation's, whose segments
        // are lost.
        if !stack.continuations {
            code.extend([
                I::GlobalGet(stack.limit),
                I::I32Const(stack.base as i32),
                I::I32Ne,
                I::If(wasm_encoder::BlockType::Empty),
                I::GlobalGet(stack.limit),
                I::I32Const(stack.base as i32),
                I::Call(stack.release),
                I::End,
            ]);
        }
        code.extend([
            I::I32Const(stack.top as i32),
            I::GlobalSet(stack.sp),
            I::I32Const(stack.base as i32),
            I::GlobalSet(stack.limit),
        ]);
    }
    code.push(I::End);
    (
        ty,
        func::Body {
            locals: Vec::new(),
            code,
        },
    )
}

/// Every type, in the order they were made.
fn type_section(cx: &Cx<'_>) -> TypeSection {
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
    types
}

/// The function table, then the tables of Wasm continuations,
/// `cont_tables`, then the table of host values.
fn table_section(cx: &Cx<'_>, cont_tables: &[u32]) -> TableSection {
    // Always a table, even an empty one, since a `perform` with no handler
    // in the module still calls through it, after trapping.
    let kinds = cx.rt.as_ref().map_or(0, |rt| rt.keys.len() as u64);
    let n_entries = u64::from(cx.wrapper_slots)
        + cx.entries.len() as u64
        + if cx.rt.is_some() {
            2 * cx.entries.len() as u64
                + 3 * kinds
                + 2 * u64::from(cx.wrapper_slots - cx.clause_slots)
        } else {
            0
        };
    let mut tables = TableSection::new();
    tables.table(TableType {
        element_type: RefType::FUNCREF,
        table64: false,
        minimum: n_entries,
        maximum: Some(n_entries),
        shared: false,
    });
    for &ct in cont_tables {
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
    if let Some(hosts) = cx.hosts {
        debug_assert_eq!(hosts.table as usize, 1 + cont_tables.len());
        tables.table(TableType {
            element_type: RefType::EXTERNREF,
            table64: false,
            minimum: 0,
            maximum: None,
            shared: false,
        });
    }
    tables
}

/// The globals: the heap pointer, the handler stack, the stack of the
/// `perform` whose clause runs, and, when counting, the live objects and
/// reference counting's own; then the shadow stack's, the free lists of
/// slots, and that of host values.
fn global_section(cx: &Cx<'_>, heap_base: u32) -> GlobalSection {
    let mut inits: Vec<i32> = if cx.rt.is_some() {
        vec![heap_base as i32, 0, 0, 0, 0, 0, 0, 0, 0]
    } else {
        vec![heap_base as i32, 0, 0]
    };
    if let Some(stack) = cx.stack {
        debug_assert_eq!(stack.sp as usize, inits.len());
        inits.extend([stack.top as i32, stack.base as i32, 0]);
    }
    if let Some(lists) = &cx.slot_lists {
        debug_assert_eq!(lists.spare as usize, inits.len());
        inits.extend(std::iter::repeat_n(0, 1 + lists.heads.len()));
    }
    if let Some(free) = cx.hosts.and_then(|h| h.free) {
        debug_assert_eq!(free as usize, inits.len());
        inits.push(0);
    }
    let mut globals = GlobalSection::new();
    for init in inits {
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(init),
        );
    }
    globals
}

/// The exports: each function, through its wrapper if it has one in
/// `wrappers`, the memory, and what the runtime gives the host.
fn export_section(
    cx: &Cx<'_>,
    exports: &[(FuncId, &str)],
    wrappers: &BTreeMap<FuncId, u32>,
    reset: Option<u32>,
) -> Result<ExportSection, Error> {
    let mut section = ExportSection::new();
    for &(f, name) in exports {
        let idx = match wrappers.get(&f) {
            Some(&idx) => idx,
            None => cx.func_index(f).map_err(Error::at(Item::Func(f)))?,
        };
        section.export(name, ExportKind::Func, idx);
    }
    section.export("memory", ExportKind::Memory, 0);
    if let Some(reset) = reset {
        section.export("conduit_reset", ExportKind::Func, reset);
    }
    if let Some(cb) = &cx.callbacks {
        section.export("conduit_release", ExportKind::Func, cb.release());
    }
    if let Some(rt) = &cx.rt {
        section.export("conduit_live", ExportKind::Global, counts::LIVE);
        if rt.cycles {
            section.export("conduit_collect", ExportKind::Func, rt.collect());
        }
    }
    Ok(section)
}

/// The function of each table slot: the clauses' wrappers, the
/// continuations' wrappers and discontinue functions, then the closures'
/// wrappers, which come after every other function, and with counts what
/// frees them and steps through what they hold. And the trampolines, which
/// `ref.func` names.
fn element_section(cx: &Cx<'_>) -> ElementSection {
    let mut refs: Vec<u32> = (0..cx.clause_slots).map(|s| cx.after_alloc() + s).collect();
    for sw in cx.switches.values() {
        refs.extend(sw.kont_funcs.iter().copied());
        refs.extend(sw.disc_funcs.iter().copied());
    }
    let first_entry = cx.after_alloc() + cx.clause_slots + cx.switch_funcs;
    refs.extend((0..cx.entries.len() as u32).map(|k| first_entry + k));
    if let Some(rt) = &cx.rt {
        let n = cx.entries.len() as u32;
        let first_free = first_entry + n;
        refs.extend((0..n).map(|k| first_free + k));
        refs.extend(rt.keys.iter().map(|k| rt.drop(k, false)));
        refs.extend((0..n).map(|k| first_free + n + k));
        refs.extend(rt.keys.iter().map(|k| rt.children(k)));
        refs.extend(rt.keys.iter().map(|k| rt.step(k)));
        // Each continuation slot's free function, then its children, the
        // discontinue slots' too, which nothing calls.
        let lists: [fn(&Switch) -> &Vec<u32>; 2] = [|sw| &sw.kont_frees, |sw| &sw.kont_children];
        for pick in lists {
            for sw in cx.switches.values() {
                refs.extend(pick(sw).iter().copied());
                if !sw.disc_slots.is_empty() {
                    refs.extend(pick(sw).iter().copied());
                }
            }
        }
    }
    let mut elements = ElementSection::new();
    if !refs.is_empty() {
        elements.active(
            Some(0),
            &ConstExpr::i32_const(0),
            Elements::Functions(refs.into()),
        );
    }
    let mut tramps: Vec<u32> = cx.switches.values().map(|sw| sw.tramp).collect();
    if let Some(cb) = &cx.callbacks {
        tramps.extend(cb.first..cb.release());
    }
    if !tramps.is_empty() {
        elements.declared(Elements::Functions(tramps.into()));
    }
    elements
}

/// `m` with a function for each operation some function discontinues a
/// continuation with: it takes the operation's arguments and performs it.
fn with_thunks(m: &Module) -> (Module, Thunks) {
    let mut ops = BTreeSet::new();
    for f in m.funcs.values() {
        for l in f.body.all_lets() {
            if let (conduit_ir::Op::Discontinue(e, i), Some(conduit_ir::Atom::Var(k))) =
                (&l.op, l.args.first())
                && matches!(f.vars.get(k.0 as usize), Some(Ty::Cont(_)))
            {
                ops.insert((*e, *i));
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
    let first_func = cx.after_alloc() + cx.wrapper_slots;
    let mut func = first_func;
    let mut slot = cx.wrapper_slots;
    let mut table = 1;
    let take = |next: &mut u32, n: u32| {
        let range: Vec<u32> = (*next..*next + n).collect();
        *next += n;
        range
    };
    for &h in switching {
        let n = cx.m.handlers[&h].len() as u32;
        let start = take(&mut func, 1)[0];
        let tramp = take(&mut func, 1)[0];
        let drive = take(&mut func, n);
        let kont_funcs = take(&mut func, n);
        let (throw, disc_funcs) = if disc {
            (take(&mut func, n), take(&mut func, n))
        } else {
            (Vec::new(), Vec::new())
        };
        let (kont_frees, kont_children, cancel) = if cx.rt.is_some() {
            (take(&mut func, n), take(&mut func, n), take(&mut func, n))
        } else {
            (Vec::new(), Vec::new(), Vec::new())
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
                cancel,
                kont_slots,
                disc_slots,
                conts,
                kont_funcs,
                disc_funcs,
                kont_frees,
                kont_children,
            },
        );
    }
    cx.wrapper_slots = slot;
    let mut tag = cx.abort_tag + u32::from(!cx.aborts.is_empty());
    if disc {
        cx.disc_tag = Some(tag);
        tag += 1;
    }
    if cx.rt.is_some() && !switching.is_empty() {
        cx.cancel_tag = Some(tag);
        tag += 1;
    }
    let effects: BTreeSet<EffectId> = (switching.iter())
        .flat_map(|h| cx.m.handlers[h].effects())
        .collect();
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
                    None => match conduit_ir::const_value(cx.m, *id) {
                        Some(ConstValue::Lit(l)) => lit_bytes(*l),
                        _ => return Err(format!("unknown constant {id}")),
                    },
                },
                nested => lay_out_value(cx, data, nested, ty)?.to_le_bytes().to_vec(),
            })
        };
    match (value, ty) {
        (ConstValue::Array(elems), Ty::Array(..)) => {
            let elem = conduit_ir::types::elem_ty(ty).unwrap();
            let size = Storage::of(&elem).size();
            let mut bytes = vec![0u8; 8 + size as usize * elems.len()];
            bytes[0..4].copy_from_slice(&immortal(cx));
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
                .map(|f| conduit_ir::types::resolve(f, *m, args, rows))
                .collect();
            let (offsets, size) = layout(&tys);
            let mut bytes = vec![0u8; size as usize];
            bytes[0..4].copy_from_slice(&immortal(cx));
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
        // A closure with no captures: its entry's slot at 4.
        (ConstValue::Closure(f), _) => {
            let slot = cx.entry(*f, 0);
            let mut bytes = immortal(cx).to_vec();
            bytes.extend(slot.to_le_bytes());
            Ok(place(data, bytes))
        }
        // As `box` makes them. Without counts, a reference is its own
        // `any`; with them, the kind at 4 is 0, as an immortal box never
        // drops what it holds.
        (ConstValue::Box(t, inner), _) => {
            if cx.rt.is_none() && !t.is_scalar() {
                return lay_out_value(cx, data, inner, t);
            }
            let s = Storage::of(t);
            let b = slot(cx, data, inner, t)?;
            let mut bytes = vec![0u8; 16];
            bytes[0..4].copy_from_slice(&immortal(cx));
            bytes[8..8 + s.size() as usize].copy_from_slice(&b[..s.size() as usize]);
            Ok(place(data, bytes))
        }
        _ => Err(format!("a constant's value {value:?} isn't a {ty}")),
    }
}

/// A constant's header: immortal when counting, so that its count, its
/// sign bit set, is never changed.
fn immortal(cx: &Cx<'_>) -> [u8; 4] {
    if cx.rt.is_some() { 0x8000_0000u32 } else { 0 }.to_le_bytes()
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

/// Names for tools and stack traces: the externs', functions', `alloc`'s
/// and the export wrappers'.
fn name_section(
    m: &Module,
    cx: &Cx<'_>,
    export_index: &BTreeMap<FuncId, u32>,
) -> Option<NameSection> {
    let mut functions = NameMap::new();
    for (i, ext) in m.externs.values().enumerate() {
        if let Some(name) = &ext.debug_name {
            functions.append(i as u32, name);
        }
    }
    let first = m.externs.len() as u32 + u32::from(cx.callbacks.is_some());
    for (i, f) in m.funcs.values().enumerate() {
        if let Some(name) = &f.debug.name {
            functions.append(first + i as u32, name);
        }
    }
    functions.append(cx.alloc, "alloc");
    let mut wrappers: Vec<(u32, FuncId)> = export_index.iter().map(|(&f, &i)| (i, f)).collect();
    wrappers.sort();
    for (i, f) in wrappers {
        let name = m.funcs[&f]
            .debug
            .name
            .as_deref()
            .map_or_else(|| f.to_string(), str::to_string);
        functions.append(i, &format!("{name}_export"));
    }
    let mut section = NameSection::new();
    section.functions(&functions);
    Some(section)
}

#[cfg(test)]
mod tests;

/// The effect and operation of clause `slot` of `decl`, counting all its
/// clauses in order, as frames and stack-switching functions number them,
/// and the clause.
fn clause_at(decl: &conduit_ir::HandlerDecl, slot: u32) -> (EffectId, u32, FuncId) {
    decl.clauses()
        .nth(slot as usize)
        .expect("a clause for every slot")
}

/// The signature of the operation of clause `slot` of `decl`.
fn op_at<'m>(m: &'m Module, decl: &conduit_ir::HandlerDecl, slot: u32) -> &'m conduit_ir::Sig {
    let (e, i, _) = clause_at(decl, slot);
    &m.effects[&e].ops[i as usize]
}
