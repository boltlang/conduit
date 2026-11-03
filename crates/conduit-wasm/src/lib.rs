//! Lowers Conduit IR directly to WebAssembly, using Wasm GC for objects
//! and tail calls for `jump ret` after a call. See "Lowering" and
//! "Operations per target" in `docs/ir.md`.
//!
//! The output needs the GC, function references and tail call proposals,
//! all part of Wasm 3.0. Not supported yet: effect handlers (`Handle`,
//! `Perform`, `Resume` and continuation types). Handlers can be
//! lowered by evidence passing before this.
//!
//! [`emit_module`] lowers a whole module at once. Each extern becomes an
//! import of its `module` and `name`, and functions are exported under the
//! names given.

mod func;
mod types;

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use conduit_ir::verify::Item;
use conduit_ir::{ExternId, FuncId, Module};
use wasm_encoder::{
    CodeSection, ElementSection, Elements, EntityType, ExportKind, ExportSection, Function,
    FunctionSection, ImportSection, TypeSection,
};

use crate::types::{FnSig, Types};

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
}

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
        let first = (self.externs.len() + self.funcs.len()) as u32;
        Ok((first + k, captures))
    }
}

/// Lowers a module that has passed [`conduit_ir::verify::verify_module`]
/// to a Wasm binary, exporting each function in `exports` under its name.
pub fn emit_module(m: &Module, exports: &[(FuncId, &str)]) -> Result<Vec<u8>, Error> {
    let mut cx = Cx {
        m,
        types: Types::new(m),
        externs: m.externs.keys().zip(0..).map(|(&e, i)| (e, i)).collect(),
        funcs: BTreeMap::new(),
        entries: Vec::new(),
        entry_keys: HashMap::new(),
    };
    let first = m.externs.len() as u32;
    cx.funcs = m.funcs.keys().zip(first..).map(|(&f, i)| (f, i)).collect();

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
        let sig = cx.types.sig(&f.sig()).map_err(err)?;
        let body = func::lower_func(&mut cx, f).map_err(err)?;
        funcs.push((plain(sig), body));
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
    module.section(&export_section);
    if entry_count > 0 {
        // `ref.func` may only name functions declared in an element segment.
        let first = first + m.funcs.len() as u32;
        let refs: Vec<u32> = (first..first + entry_count as u32).collect();
        let mut elements = ElementSection::new();
        elements.declared(Elements::Functions(refs.into()));
        module.section(&elements);
    }
    module.section(&code_section);
    Ok(module.finish())
}

#[cfg(test)]
mod tests;
