//! The IR of Conduit: a typed, monomorphic, A-normal-form tree with join
//! points. See `docs/ir.md` for the design.
//!
//! A pass handles 5 kinds of [`Term`]. Everything that isn't control flow is
//! a [`Term::Let`] holding an [`Op`], so a pass that doesn't care about an
//! operation never has to match on it.
//!
//! Every type here is plain data without lifetimes: it derives `Clone`, `Eq`
//! and `Hash`, and refers to other items by opaque, stable IDs (see [`id`]),
//! never by position. A frontend can therefore cache IR in an incremental
//! framework like Salsa without Conduit depending on it.

pub mod id;
mod module;
mod op;
mod term;
mod ty;

use std::sync::Arc;

pub use id::{CtorId, DataId, EffectId, ExternId, FuncId, HandlerId, Label, SourceId, Var};
pub use module::{DataDecl, EffectDecl, Extern, Func, HandlerDecl, Module};
pub use op::{Effects, Op, Prim, Sign};
pub use term::{Arm, Atom, JoinDef, Lit, Tag, Term};
pub use ty::{CtorDecl, Mutability, Sig, Ty};

/// How passes reach items other than the function they work on.
///
/// The frontend implements this. A frontend using Salsa implements it with
/// tracked queries, so that a pass asking for a callee while inlining
/// records a dependency on it.
pub trait Program {
    fn func(&self, id: FuncId) -> Arc<Func>;
    fn data(&self, id: DataId) -> Arc<DataDecl>;
}

#[cfg(test)]
mod tests;
