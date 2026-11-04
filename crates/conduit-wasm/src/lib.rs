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
//! Other clauses, and continuation types, need stack switching or evidence
//! passing, which aren't supported yet.
//!
//! [`emit_module`] lowers a whole module at once. Each extern becomes an
//! import of its `module` and `name`, and functions are exported under the
//! names given.

mod func;
mod types;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use conduit_ir::verify::Item;
use conduit_ir::{EffectId, ExternId, Func, FuncId, HandlerId, Module};
use wasm_encoder::{
    CodeSection, ConstExpr, ElementSection, Elements, EntityType, ExportKind, ExportSection,
    Function, FunctionSection, GlobalSection, GlobalType, HeapType, ImportSection, TagKind,
    TagSection, TagType, TypeSection,
};

use crate::types::{FnSig, Types, nullable};

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
        let first = (self.externs.len() + self.funcs.len()) as u32 + self.wrapper_count();
        Ok((first + k, captures))
    }
}

/// Lowers a module that has passed [`conduit_ir::verify::verify_module`]
/// to a Wasm binary, exporting each function in `exports` under its name.
pub fn emit_module(m: &Module, exports: &[(FuncId, &str)]) -> Result<Vec<u8>, Error> {
    // Tail-resumptive and aborting clauses are lowered as plain functions,
    // without their continuation. Any other clause keeps its continuation
    // parameter, which has no Wasm type.
    let mut plain_clauses: BTreeMap<FuncId, Func> = BTreeMap::new();
    let mut not_plain = BTreeMap::new();
    let mut aborts = BTreeSet::new();
    let mut aborting = BTreeSet::new();
    let mut discontinues = false;
    for (&h, decl) in &m.handlers {
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
            } else {
                not_plain.insert(clause, h);
            }
        }
    }

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
    };
    cx.types.aborting = aborting;
    let first = m.externs.len() as u32;
    cx.funcs = m.funcs.keys().zip(first..).map(|(&f, i)| (f, i)).collect();

    let mut next_wrapper = first + m.funcs.len() as u32;
    for (&h, decl) in &m.handlers {
        if decl.ops.iter().all(|c| plain_clauses.contains_key(c)) {
            cx.wrappers.insert(h, next_wrapper);
            next_wrapper += decl.ops.len() as u32;
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
        if let Some(h) = not_plain.get(&id) {
            return Err(err(format!(
                "a clause of {h} that isn't tail-resumptive or aborting, which needs \
                 stack switching or evidence passing"
            )));
        }
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
    if let Some(sig) = abort {
        let mut tags = TagSection::new();
        tags.tag(TagType {
            kind: TagKind::Exception,
            func_type_idx: group_len + sig,
        });
        module.section(&tags);
    }
    if let Some(frame) = handlers {
        let mut globals = GlobalSection::new();
        let count = if cx.discontinues { 2 } else { 1 };
        for _ in 0..count {
            globals.global(
                GlobalType {
                    val_type: nullable(frame),
                    mutable: true,
                    shared: false,
                },
                &ConstExpr::ref_null(HeapType::Concrete(frame)),
            );
        }
        module.section(&globals);
    }
    module.section(&export_section);
    let referenced = wrappers.len() + entry_count;
    if referenced > 0 {
        // `ref.func` may only name functions declared in an element segment.
        let first = first + m.funcs.len() as u32;
        let refs: Vec<u32> = (first..first + referenced as u32).collect();
        let mut elements = ElementSection::new();
        elements.declared(Elements::Functions(refs.into()));
        module.section(&elements);
    }
    module.section(&code_section);
    Ok(module.finish())
}

#[cfg(test)]
mod tests;
