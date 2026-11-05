//! Lowers Conduit IR directly to WebAssembly, using Wasm GC for objects
//! and tail calls for `jump ret` after a call. See "Lowering" and
//! "Operations per target" in `docs/ir.md`.
//!
//! The output needs the GC, function references and tail call proposals,
//! all part of Wasm 3.0.
//!
//! Effect handlers are supported when every clause of the handlers used is
//! tail-resumptive or aborting (see [`conduit_ir::clause`]): then no
//! continuation is ever captured. A global holds a stack of handler frames.
//! `handle` pushes one, runs the body and pops it, and `perform` finds the
//! nearest frame of its effect and calls the clause, with the stack below
//! that frame while the clause runs. A `perform` with no handler traps.
//!
//! An aborting clause stores what it gives in its frame and throws an
//! exception holding the frame, which the `handle` that pushed it catches:
//! the output then also needs exception handling, also part of Wasm 3.0.
//!
//! A tail-resumptive clause may also end in `discontinue E.i(k, xs)`
//! instead of `resume`. It then performs `E.i(xs)` itself, under the
//! handler stack of the `perform` it handles, which a second global holds
//! while the clause runs, and gives what that gives.
//!
//! A handler with any other clause switches stacks, with the stack
//! switching proposal, which isn't standard yet: see `switch.rs`. Its body
//! runs as a Wasm continuation, `perform` suspends to it when its frame is
//! the nearest of the effect, and a Conduit continuation is a closure that
//! resumes the Wasm one under the handler again. Only modules with such a
//! handler need the proposal.
//!
//! Each constant is an immutable global, made before the program runs by
//! the global's initializer. A constant's arrays are made with
//! `array.new_fixed`, which engines limit: V8 to 10,000 elements.
//!
//! [`emit_module`] lowers a whole module at once. Each extern becomes an
//! import of its `module` and `name`, and functions are exported under the
//! names given.

mod consts;
mod func;
mod switch;
mod types;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use conduit_ir::verify::Item;
use conduit_ir::{
    Atom, Block, ConstId, EffectId, ExternId, Func, FuncId, HandlerId, Label, Let, Module, Op, Row,
    Tail, Ty,
};
use wasm_encoder::{
    CodeSection, ConstExpr, ElementSection, Elements, EntityType, ExportKind, ExportSection,
    Function, FunctionSection, GlobalSection, GlobalType, HeapType, ImportSection, IndirectNameMap,
    NameMap, NameSection, TagKind, TagSection, TagType, TypeSection,
};

use crate::types::{FnSig, Types, nullable};
use wasm_encoder::{RefType, ValType};

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

/// State shared by the functions of a module.
pub(crate) struct Cx<'m> {
    m: &'m Module,
    types: Types<'m>,
    funcs: BTreeMap<FuncId, u32>,
    externs: BTreeMap<ExternId, u32>,
    /// The wrappers closures call: a function and how many of its
    /// parameters are captured. They come after the module's functions.
    entries: Vec<(FuncId, usize)>,
    entry_keys: HashMap<(FuncId, usize), u32>,
    /// The handlers whose clauses are all tail-resumptive or aborting, and
    /// the index of the wrapper of their first clause: a function of the
    /// clause type that reads the environment out of the frame and calls
    /// the clause. They come after the module's functions.
    wrappers: BTreeMap<HandlerId, u32>,
    /// The aborting clauses.
    aborts: BTreeSet<FuncId>,
    /// Whether some clause discontinues, which needs [`SITE`].
    discontinues: bool,
    /// The global of each constant. They come after the handler stack's.
    consts: BTreeMap<ConstId, u32>,
    /// The functions of each handler that switches stacks.
    switches: BTreeMap<HandlerId, Switch>,
    /// The control tag of each operation of an effect that a handler that
    /// switches stacks handles.
    tags: BTreeMap<(EffectId, u32), u32>,
    /// The exception that `discontinue` throws into a continuation, which
    /// holds a closure that performs the operation.
    disc_tag: Option<u32>,
    /// For each operation discontinued with a continuation: the function
    /// that performs it, and the wrapper of the closure that calls that.
    thunks: BTreeMap<(EffectId, u32), (u32, u32)>,
    /// How many functions [`Cx::switches`] and [`Cx::thunks`] hold.
    switch_count: u32,
}

/// The functions of a handler that switches stacks. See `switch.rs`.
pub(crate) struct Switch {
    /// Runs a body under the handler: `(frame, env.., body) -> R`.
    pub start: u32,
    /// For each operation, resumes its continuation under the handler:
    /// `(frame, env.., k, results..) -> R`.
    pub drive: Vec<u32>,
    /// For each operation, the wrapper of the Conduit continuation its
    /// clause gets, which calls `drive`.
    pub entry: Vec<u32>,
    /// For each operation, if some clause discontinues: throws a closure
    /// into its continuation under the handler: `(frame, env.., k, thunk)
    /// -> R`.
    pub throw: Vec<u32>,
    /// For each operation, if some clause discontinues: what the Conduit
    /// continuation calls to discontinue, which calls `throw`.
    pub disc: Vec<u32>,
}

/// The exception an aborting clause throws, holding its handler's frame.
pub(crate) const ABORT: u32 = 0;

/// The global holding the top of the handler stack.
pub(crate) const HANDLERS: u32 = 0;

/// The global holding the handler stack of the `perform` whose clause is
/// running, under which a `discontinue` in that clause performs. Only there
/// when some clause discontinues.
pub(crate) const SITE: u32 = 1;

impl Cx<'_> {
    fn func_index(&self, f: FuncId) -> Result<u32, String> {
        self.funcs
            .get(&f)
            .copied()
            .ok_or_else(|| format!("unknown function {f}"))
    }

    fn const_global(&self, c: ConstId) -> Result<u32, String> {
        self.consts
            .get(&c)
            .copied()
            .ok_or_else(|| format!("unknown constant {c}"))
    }

    /// The control tag of operation `i` of `e`, if a handler of `e`
    /// switches stacks.
    fn tag(&self, e: EffectId, i: u32) -> Option<u32> {
        self.tags.get(&(e, i)).copied()
    }

    fn switch(&self, h: HandlerId) -> Option<&Switch> {
        self.switches.get(&h)
    }

    fn extern_index(&self, e: ExternId) -> Result<u32, String> {
        self.externs
            .get(&e)
            .copied()
            .ok_or_else(|| format!("unknown extern {e}"))
    }

    /// The number of an effect, which its frames hold.
    fn effect_number(&self, e: EffectId) -> Result<i32, String> {
        self.m
            .effects
            .keys()
            .position(|&k| k == e)
            .map(|i| i as i32)
            .ok_or_else(|| format!("unknown effect {e}"))
    }

    /// The wrapper of clause `i` of `h`.
    fn wrapper(&self, h: HandlerId, i: u32) -> Result<u32, String> {
        match self.wrappers.get(&h) {
            Some(first) => Ok(first + i),
            None => Err(format!(
                "{h} has a clause that isn't tail-resumptive or aborting, which needs \
                 stack switching or evidence passing"
            )),
        }
    }

    fn wrapper_count(&self) -> u32 {
        self.wrappers
            .keys()
            .map(|h| self.m.handlers[h].ops.len() as u32)
            .sum()
    }

    /// The wrapper of `f`'s closures with `n` captured values, and the type
    /// of those closures.
    fn entry(&mut self, f: FuncId, n: usize) -> Result<(u32, u32), String> {
        let (captures, _) = self.types.captures(f, n)?;
        let k = match self.entry_keys.get(&(f, n)) {
            Some(&k) => k,
            None => {
                let k = self.entries.len() as u32;
                self.entries.push((f, n));
                self.entry_keys.insert((f, n), k);
                k
            }
        };
        let first = (self.externs.len() + self.funcs.len()) as u32
            + self.wrapper_count()
            + self.switch_count;
        Ok((first + k, captures))
    }
}

/// Lowers a module that has passed [`conduit_ir::verify::verify_module`]
/// to a Wasm binary, exporting each function in `exports` under its name.
pub fn emit_module(m: &Module, exports: &[(FuncId, &str)]) -> Result<Vec<u8>, Error> {
    // Tail-resumptive and aborting clauses are lowered as plain functions,
    // without their continuation, when every clause of their handler is
    // one. A handler with any other clause switches stacks, and its
    // clauses keep their continuation.
    let mut plain_clauses: BTreeMap<FuncId, Func> = BTreeMap::new();
    let mut aborts = BTreeSet::new();
    let mut aborting = BTreeSet::new();
    let mut switching = BTreeSet::new();
    let mut discontinues = false;
    for (&h, decl) in &m.handlers {
        let mut plain = Vec::new();
        for &clause in &decl.ops {
            let Some(f) = m.funcs.get(&clause) else {
                continue;
            };
            if let Some(p) = conduit_ir::clause::plain_call(f) {
                plain.push((clause, p, false));
            } else if let Some(p) = conduit_ir::clause::aborting(f) {
                plain.push((clause, p, true));
            } else {
                switching.insert(h);
            }
        }
        if switching.contains(&h) {
            continue;
        }
        for (clause, p, abort) in plain {
            if abort {
                aborts.insert(clause);
                aborting.insert(h);
            } else {
                discontinues |= conduit_ir::clause::discontinues(&p);
            }
            plain_clauses.insert(clause, p);
        }
    }
    for &h in &switching {
        if let Some(&c) = m.handlers[&h]
            .ops
            .iter()
            .find(|c| plain_clauses.contains_key(c))
        {
            return Err(Error {
                item: Item::Func(c),
                message: format!(
                    "a clause of {h}, which switches stacks, and of a handler that doesn't"
                ),
            });
        }
    }
    // The operations discontinued with a continuation, which needs each
    // continuation to be able to throw into its Wasm continuation.
    let thunk_ops = cont_discontinues(m, &plain_clauses);
    let thunk_funcs: Vec<((EffectId, u32), Func)> = thunk_ops
        .iter()
        .map(|&(e, i)| ((e, i), thunk_func(m, e, i)))
        .collect();

    let mut cx = Cx {
        m,
        types: Types::new(m),
        externs: m.externs.keys().zip(0..).map(|(&e, i)| (e, i)).collect(),
        funcs: BTreeMap::new(),
        entries: Vec::new(),
        entry_keys: HashMap::new(),
        wrappers: BTreeMap::new(),
        aborts,
        discontinues,
        consts: BTreeMap::new(),
        switches: BTreeMap::new(),
        tags: BTreeMap::new(),
        disc_tag: None,
        thunks: BTreeMap::new(),
        switch_count: 0,
    };
    cx.types.aborting = aborting;
    cx.types.switching = switching.clone();
    cx.types.discontinues = !thunk_ops.is_empty();
    let handler_globals = match (m.effects.is_empty(), discontinues) {
        (true, _) => 0,
        (false, false) => 1,
        (false, true) => 2,
    };
    // A global's initializer can only read the globals before it, so a
    // constant comes after those it's made of.
    let const_order = consts::order(m);
    cx.consts = const_order
        .iter()
        .zip(handler_globals..)
        .map(|(&c, i)| (c, i))
        .collect();
    let first = m.externs.len() as u32;
    cx.funcs = m.funcs.keys().zip(first..).map(|(&f, i)| (f, i)).collect();

    let mut next_wrapper = first + m.funcs.len() as u32;
    for (&h, decl) in &m.handlers {
        if !switching.contains(&h) && decl.ops.iter().all(|c| plain_clauses.contains_key(c)) {
            cx.wrappers.insert(h, next_wrapper);
            next_wrapper += decl.ops.len() as u32;
        }
    }

    // The functions of handlers that switch stacks, then those of the
    // closures `discontinue` throws, come after the wrappers.
    let mut next = next_wrapper;
    let mut take = |k: u32| {
        let range: Vec<u32> = (next..next + k).collect();
        next += k;
        range
    };
    for &h in &switching {
        let n = m
            .effects
            .get(&m.handlers[&h].effect)
            .map_or(0, |e| e.ops.len()) as u32;
        let start = take(1)[0];
        let drive = take(n);
        let entry = take(n);
        let (throw, disc) = if thunk_ops.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            (take(n), take(n))
        };
        cx.switches.insert(
            h,
            Switch {
                start,
                drive,
                entry,
                throw,
                disc,
            },
        );
    }
    for &op in &thunk_ops {
        let fs = take(2);
        cx.thunks.insert(op, (fs[0], fs[1]));
    }
    cx.switch_count = next - next_wrapper;

    // Tags: the exception of aborting clauses, the one `discontinue`
    // throws into a continuation, then a control tag for each operation
    // of an effect a handler that switches stacks handles.
    let mut next_tag = u32::from(!cx.aborts.is_empty());
    if !thunk_ops.is_empty() {
        cx.disc_tag = Some(next_tag);
        next_tag += 1;
    }
    let switch_effects: BTreeSet<EffectId> =
        switching.iter().map(|h| m.handlers[h].effect).collect();
    let mut control_tags = Vec::new();
    for &e in &switch_effects {
        let Some(decl) = m.effects.get(&e) else {
            continue;
        };
        for i in 0..decl.ops.len() as u32 {
            cx.tags.insert((e, i), next_tag);
            control_tags.push((e, i));
            next_tag += 1;
        }
    }

    // Function types of imports and functions are outside the group, so
    // that the host sees ordinary types like `(func (param i32))`.
    let mut sigs: Vec<FnSig> = Vec::new();
    let mut sig_keys: HashMap<FnSig, u32> = HashMap::new();
    let mut plain = |sig: FnSig| -> u32 {
        *sig_keys.entry(sig.clone()).or_insert_with(|| {
            sigs.push(sig);
            sigs.len() as u32 - 1
        })
    };

    let mut imports = Vec::new();
    for (&id, ext) in &m.externs {
        let sig = cx.types.sig(&ext.sig).map_err(|message| Error {
            item: Item::Extern(id),
            message,
        })?;
        imports.push((ext, plain(sig)));
    }

    let mut funcs = Vec::new();
    for (&id, f) in &m.funcs {
        let err = |message| Error {
            item: Item::Func(id),
            message,
        };
        let f = plain_clauses.get(&id).unwrap_or(f);
        let sig = cx.types.sig(&f.sig()).map_err(err)?;
        let body = func::lower_func(&mut cx, f).map_err(err)?;
        funcs.push((plain(sig), body));
    }

    let mut wrappers = Vec::new();
    for h in cx.wrappers.keys().copied().collect::<Vec<_>>() {
        let decl = &m.handlers[&h];
        for i in 0..decl.ops.len() as u32 {
            let err = |message| Error {
                item: Item::Handler(h),
                message,
            };
            let ty = cx.types.clause(decl.effect, i).map_err(err)?;
            let body = func::lower_wrapper(&mut cx, h, i).map_err(err)?;
            wrappers.push((ty, body));
        }
    }

    // Each is a plain type, after the group, or one in the group.
    let mut switched: Vec<(bool, u32, func::Body)> = Vec::new();
    for &h in &switching {
        let err = |message| Error {
            item: Item::Handler(h),
            message,
        };
        let n = cx.switches[&h].drive.len() as u32;
        let disc = !cx.switches[&h].disc.is_empty();
        let mut starts = vec![switch::Start::Body];
        starts.extend((0..n).map(switch::Start::Resume));
        for start in starts {
            let sig = switch::drive_sig(&mut cx, h, start).map_err(err)?;
            let body = switch::lower_drive(&mut cx, h, start).map_err(err)?;
            switched.push((true, plain(sig), body));
        }
        for i in 0..n {
            let sig = cx.types.clause_cont_sig(h, i).map_err(err)?;
            let ty = cx.types.entry(&sig).map_err(err)?;
            let body = switch::lower_cont_entry(&mut cx, h, i, false).map_err(err)?;
            switched.push((false, ty, body));
        }
        if disc {
            for i in 0..n {
                let start = switch::Start::Throw(i);
                let sig = switch::drive_sig(&mut cx, h, start).map_err(err)?;
                let body = switch::lower_drive(&mut cx, h, start).map_err(err)?;
                switched.push((true, plain(sig), body));
            }
            for i in 0..n {
                let sig = cx.types.clause_cont_sig(h, i).map_err(err)?;
                let ty = cx.types.discontinue(&sig).map_err(err)?;
                let body = switch::lower_cont_entry(&mut cx, h, i, true).map_err(err)?;
                switched.push((false, ty, body));
            }
        }
    }
    for ((e, i), f) in &thunk_funcs {
        let err = |message| Error {
            item: Item::Effect(*e),
            message,
        };
        let sig = cx.types.sig(&f.sig()).map_err(err)?;
        let body = func::lower_func(&mut cx, f).map_err(err)?;
        switched.push((true, plain(sig), body));
        let (_, thunk_sig) = cx.types.thunk(*e, *i).map_err(err)?;
        let ty = cx.types.entry(&thunk_sig).map_err(err)?;
        let body = switch::lower_thunk_entry(&mut cx, *e, *i).map_err(err)?;
        switched.push((false, ty, body));
    }
    // Tag types: plain function types.
    let disc_tag_ty = cx.disc_tag.map(|_| {
        plain(FnSig {
            params: vec![ValType::Ref(RefType::ANYREF)],
            results: Vec::new(),
        })
    });
    let mut control_tag_tys = Vec::new();
    for &(e, i) in &control_tags {
        let err = |message| Error {
            item: Item::Effect(e),
            message,
        };
        let op = &m.effects[&e].ops[i as usize];
        let sig = cx.types.sig(op).map_err(err)?;
        control_tag_tys.push(plain(sig));
    }

    let mut entries = Vec::new();
    for k in 0..cx.entries.len() {
        let (f, n) = cx.entries[k];
        let err = |message| Error {
            item: Item::Func(f),
            message,
        };
        let (_, sig) = cx.types.captures(f, n).map_err(err)?;
        let ty = cx.types.entry(&sig).map_err(err)?;
        let body = func::lower_entry(&mut cx, f, n).map_err(err)?;
        entries.push((ty, body));
    }

    let mut export_section = ExportSection::new();
    for &(f, name) in exports {
        let idx = cx.func_index(f).map_err(|message| Error {
            item: Item::Func(f),
            message,
        })?;
        export_section.export(name, ExportKind::Func, idx);
    }

    let mut const_globals = Vec::new();
    for &id in &const_order {
        let err = |message| Error {
            item: Item::Const(id),
            message,
        };
        let decl = &m.consts[&id];
        let ty = cx.types.val(&decl.ty).map_err(err)?;
        let init = consts::init(&mut cx, &decl.value, &decl.ty).map_err(err)?;
        const_globals.push((ty, init));
    }

    let entry_count = cx.entries.len();
    let handlers = if m.effects.is_empty() {
        None
    } else {
        Some(cx.types.frame().map_err(|message| Error {
            item: Item::Effect(*m.effects.keys().next().unwrap()),
            message,
        })?)
    };
    let abort = match handlers {
        Some(frame) if !cx.aborts.is_empty() => Some(plain(FnSig {
            params: vec![nullable(frame)],
            results: vec![],
        })),
        _ => None,
    };
    let group = cx.types.finish();
    let group_len = group.len() as u32;

    let mut type_section = TypeSection::new();
    if !group.is_empty() {
        type_section.ty().rec(group);
    }
    for sig in &sigs {
        type_section
            .ty()
            .function(sig.params.iter().copied(), sig.results.iter().copied());
    }

    let mut import_section = ImportSection::new();
    for (ext, sig) in &imports {
        import_section.import(
            &ext.module,
            &ext.name,
            EntityType::Function(group_len + sig),
        );
    }

    let mut function_section = FunctionSection::new();
    let mut code_section = CodeSection::new();
    let bodies = funcs
        .iter()
        .map(|(sig, body)| (group_len + sig, body))
        .chain(wrappers.iter().map(|(ty, body)| (*ty, body)))
        .chain(
            switched
                .iter()
                .map(|(is_plain, ty, body)| (if *is_plain { group_len + ty } else { *ty }, body)),
        )
        .chain(entries.iter().map(|(ty, body)| (*ty, body)));
    for (ty, body) in bodies {
        function_section.function(ty);
        let mut f = Function::new_with_locals_types(body.locals.iter().copied());
        for i in &body.code {
            f.instruction(i);
        }
        code_section.function(&f);
    }

    let mut module = wasm_encoder::Module::new();
    module.section(&type_section);
    module.section(&import_section);
    module.section(&function_section);
    let tag_tys: Vec<u32> = abort
        .into_iter()
        .chain(disc_tag_ty)
        .chain(control_tag_tys)
        .collect();
    if !tag_tys.is_empty() {
        let mut tags = TagSection::new();
        for sig in tag_tys {
            tags.tag(TagType {
                kind: TagKind::Exception,
                func_type_idx: group_len + sig,
            });
        }
        module.section(&tags);
    }
    if handlers.is_some() || !const_globals.is_empty() {
        let mut globals = GlobalSection::new();
        if let Some(frame) = handlers {
            for _ in 0..handler_globals {
                globals.global(
                    GlobalType {
                        val_type: nullable(frame),
                        mutable: true,
                        shared: false,
                    },
                    &ConstExpr::ref_null(HeapType::Concrete(frame)),
                );
            }
        }
        for (ty, init) in &const_globals {
            let global = GlobalType {
                val_type: *ty,
                mutable: false,
                shared: false,
            };
            globals.global(global, init);
        }
        module.section(&globals);
    }
    module.section(&export_section);
    let referenced = wrappers.len() + switched.len() + entry_count;
    if referenced > 0 {
        // `ref.func` may only name functions declared in an element segment.
        let first = first + m.funcs.len() as u32;
        let refs: Vec<u32> = (first..first + referenced as u32).collect();
        let mut elements = ElementSection::new();
        elements.declared(Elements::Functions(refs.into()));
        module.section(&elements);
    }
    module.section(&code_section);
    // The functions of handlers that switch stacks are named after them.
    let mut extra_names = Vec::new();
    for (h, switch) in &cx.switches {
        let decl = &m.handlers[h];
        let name = decl
            .debug_name
            .as_deref()
            .map_or(h.to_string(), str::to_string);
        extra_names.push((switch.start, format!("{name}.start")));
        for (kind, fs) in [
            ("drive", &switch.drive),
            ("k", &switch.entry),
            ("throw", &switch.throw),
            ("discontinue", &switch.disc),
        ] {
            for (i, &f) in fs.iter().enumerate() {
                extra_names.push((f, format!("{name}.{kind}{i}")));
            }
        }
    }
    for (&(e, i), &(body, entry)) in &cx.thunks {
        let name = m.effects[&e]
            .debug_name
            .as_deref()
            .map_or(e.to_string(), str::to_string);
        extra_names.push((body, format!("{name}.{i}.thunk")));
        extra_names.push((entry, format!("{name}.{i}.thunk_entry")));
    }
    extra_names.sort();
    if let Some(names) = name_section(m, &funcs, &extra_names) {
        module.section(&names);
    }
    Ok(module.finish())
}

/// The names of the module's externs, functions and their variables, for
/// tools and stack traces, or `None` if nothing has one. `funcs` are the
/// lowered functions, in the order of the module's.
fn name_section(
    m: &Module,
    funcs: &[(u32, func::Body)],
    extra: &[(u32, String)],
) -> Option<NameSection> {
    let mut functions = NameMap::new();
    let mut locals = IndirectNameMap::new();
    let mut any = false;
    for (i, ext) in m.externs.values().enumerate() {
        if let Some(name) = &ext.debug_name {
            functions.append(i as u32, name);
            any = true;
        }
    }
    let first = m.externs.len() as u32;
    for (i, (f, (_, body))) in m.funcs.values().zip(funcs).enumerate() {
        let index = first + i as u32;
        if let Some(name) = &f.debug.name {
            functions.append(index, name);
            any = true;
        }
        if !body.local_names.is_empty() {
            let mut names = NameMap::new();
            for (local, name) in &body.local_names {
                names.append(*local, name);
            }
            locals.append(index, &names);
            any = true;
        }
    }
    for (index, name) in extra {
        functions.append(*index, name);
        any = true;
    }
    any.then(|| {
        let mut section = NameSection::new();
        section.functions(&functions);
        section.locals(&locals);
        section
    })
}

#[cfg(test)]
mod tests;

/// The operations that some function discontinues a continuation with,
/// other than the plain clauses, whose continuation is gone.
fn cont_discontinues(m: &Module, plain: &BTreeMap<FuncId, Func>) -> BTreeSet<(EffectId, u32)> {
    let mut ops = BTreeSet::new();
    for (id, f) in &m.funcs {
        if plain.contains_key(id) {
            continue;
        }
        let mut todo = vec![&f.body];
        while let Some(b) = todo.pop() {
            for l in &b.lets {
                if let (Op::Discontinue(e, i), Some(Atom::Var(k))) = (&l.op, l.args.first())
                    && matches!(f.vars.get(k.0 as usize), Some(Ty::Cont(_)))
                {
                    ops.insert((*e, *i));
                }
            }
            match &b.tail {
                Tail::Join { defs, body, .. } => {
                    todo.extend(defs.iter().map(|d| &d.body));
                    todo.push(body);
                }
                Tail::Case { arms, default, .. } => {
                    todo.extend(arms.iter().map(|a| &a.body));
                    todo.extend(default.as_deref());
                }
                Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
            }
        }
    }
    ops
}

/// The function that the closure `discontinue E.i` throws calls: it takes
/// the operation's arguments and performs it.
fn thunk_func(m: &Module, e: EffectId, i: u32) -> Func {
    let op = &m.effects[&e].ops[i as usize];
    let mut f = Func::new(op.params.clone(), op.results.clone());
    f.row = Row::one(e);
    let results: Vec<_> = op.results.iter().map(|t| f.new_var(t.clone())).collect();
    let args = f.params.iter().map(|&p| Atom::Var(p)).collect();
    f.body = Block {
        lets: vec![Let {
            binds: results.clone(),
            op: Op::Perform(e, i),
            args,
            source: None,
        }],
        tail: Tail::Jump {
            target: Label::RET,
            args: results.into_iter().map(Atom::Var).collect(),
            source: None,
        },
    };
    f
}
