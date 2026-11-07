//! The IR of Conduit: a typed, monomorphic, A-normal-form tree with join
//! points. See `docs/ir.md` for the design.
//!
//! A function's body is a [`Block`]: a list of [`Let`]s, then a [`Tail`]
//! that ends it. So a pass handles 5 kinds of thing: a `let`, and the 4
//! kinds of tail. Everything that isn't control flow is a `let` holding an
//! [`Op`], so a pass that doesn't care about an operation never has to
//! match on it.
//!
//! Every type here is plain data without lifetimes: it derives `Clone`, `Eq`
//! and `Hash`, and refers to other items by opaque, stable IDs (see [`id`]),
//! never by position. A frontend can therefore cache IR in an incremental
//! framework like Salsa without Conduit depending on it.

pub mod build;
pub mod clause;
pub mod id;
mod module;
mod op;
mod row;
mod term;
pub mod text;
mod ty;
pub mod types;
pub mod verify;

pub use id::{
    ConstId, CtorId, DataId, EffectId, ExternId, FuncId, HandlerId, Label, SourceId, Var,
};
pub use module::{
    Abi, ConstDecl, ConstValue, DataDecl, EffectDecl, Extern, Func, FuncDebug, HandlerDecl, Link,
    Module, MutParams, const_refs, const_value,
};
pub use op::{Effects, Memory, Op, Prim, Sign};
pub use row::Row;
pub use term::{Arm, Atom, Block, Blocks, JoinDef, Let, Lit, Tag, Tail};

/// Runs `f`, on more stack if what's left is low. A function recurses as
/// deep as the blocks it walks nest, which is as deep as the function is
/// long when a frontend puts the rest of it in each `if`'s join point, so
/// every recursive walk of blocks goes through this, as rustc's do. On a
/// target where the stack can't grow, such as wasm32, it just runs `f`.
pub fn deep<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, f)
}
pub use ty::{CtorDecl, Mutability, Sig, Ty};

/// How passes reach items other than the function they work on. Each
/// lookup gives `None` for an unknown ID.
///
/// The frontend implements this. A frontend using Salsa implements it with
/// tracked queries, so that a pass asking for a callee while inlining
/// records a dependency on it. [`Module`] implements it too.
pub trait Program {
    fn func(&self, id: FuncId) -> Option<&Func>;

    /// A function's signature. Passes that only need the signature, like
    /// the verifier checking a call, ask for this instead of
    /// [`Program::func`], so that a frontend can answer without making them
    /// depend on the function's body.
    fn func_sig(&self, id: FuncId) -> Option<Sig> {
        // Not `Func::sig`, which panics on a parameter without a type: the
        // verifier asks for signatures of functions it hasn't checked yet.
        let f = self.func(id)?;
        let params = f.params.iter().map(|p| f.vars.get(p.0 as usize).cloned());
        Some(Sig {
            row: f.row.clone(),
            params: params.collect::<Option<_>>()?,
            results: f.results.clone(),
        })
    }

    /// A function's mutability parameters, which its signature can use.
    fn func_mut_params(&self, id: FuncId) -> Option<MutParams> {
        Some(self.func(id)?.mut_params.clone())
    }

    /// How many effect parameters a function has, which its signature's
    /// rows can use.
    fn func_effect_params(&self, id: FuncId) -> Option<u32> {
        Some(self.func(id)?.effect_params)
    }

    fn data(&self, id: DataId) -> Option<&DataDecl>;
    fn extern_fn(&self, id: ExternId) -> Option<&Extern>;
    fn effect(&self, id: EffectId) -> Option<&EffectDecl>;
    fn handler(&self, id: HandlerId) -> Option<&HandlerDecl>;
    fn constant(&self, id: ConstId) -> Option<&ConstDecl>;
}

impl Program for Module {
    fn func(&self, id: FuncId) -> Option<&Func> {
        self.funcs.get(&id)
    }

    fn data(&self, id: DataId) -> Option<&DataDecl> {
        self.data.get(&id)
    }

    fn extern_fn(&self, id: ExternId) -> Option<&Extern> {
        self.externs.get(&id)
    }

    fn effect(&self, id: EffectId) -> Option<&EffectDecl> {
        self.effects.get(&id)
    }

    fn handler(&self, id: HandlerId) -> Option<&HandlerDecl> {
        self.handlers.get(&id)
    }

    fn constant(&self, id: ConstId) -> Option<&ConstDecl> {
        self.consts.get(&id)
    }
}

#[cfg(test)]
mod tests;
