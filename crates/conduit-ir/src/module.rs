use std::collections::BTreeMap;

use crate::id::{DataId, EffectId, ExternId, FuncId, HandlerId, Label, Var};
use crate::row::Row;
use crate::term::{Block, Tail};
use crate::ty::{CtorDecl, Mutability, Sig, Ty};

/// A whole program, before linking.
///
/// The maps are ordered by ID, so iterating over a module is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Module {
    pub data: BTreeMap<DataId, DataDecl>,
    pub funcs: BTreeMap<FuncId, Func>,
    pub externs: BTreeMap<ExternId, Extern>,
    pub effects: BTreeMap<EffectId, EffectDecl>,
    pub handlers: BTreeMap<HandlerId, HandlerDecl>,
}

/// A top-level function. Nested functions don't exist: frontends lift them,
/// with their captured values as the first parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Func {
    /// The mutability parameters its types can use.
    pub mut_params: MutParams,
    /// The type of every variable, indexed by [`Var`].
    pub vars: Vec<Ty>,
    /// How many labels the body uses, [`Label::RET`] included.
    pub labels: u32,
    pub params: Vec<Var>,
    pub results: Vec<Ty>,
    /// The effects it may perform.
    pub row: Row,
    pub body: Block,
    /// Names for its parts, to show them by.
    pub debug: FuncDebug,
}

/// Names for a function and its variables and labels, which the text
/// format, errors and backends show instead of numbers where they can.
/// Passes don't look at them: they keep them, and give none to what they
/// make. A name needn't be unique: an inlined function's variables keep
/// theirs, and instances of one generic function share one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FuncDebug {
    pub name: Option<Box<str>>,
    pub vars: BTreeMap<Var, Box<str>>,
    pub labels: BTreeMap<Label, Box<str>>,
}

impl Func {
    /// A function with the given parameter types and nothing else, so its
    /// body is `unreachable` until it's set.
    pub fn new(params: Vec<Ty>, results: Vec<Ty>) -> Func {
        let params_len = params.len() as u32;
        Func {
            mut_params: MutParams::default(),
            vars: params,
            labels: 1,
            params: (0..params_len).map(Var).collect(),
            results,
            row: Row::NOTHING,
            body: Tail::Unreachable.into(),
            debug: FuncDebug::default(),
        }
    }

    pub fn new_var(&mut self, ty: Ty) -> Var {
        self.vars.push(ty);
        Var(self.vars.len() as u32 - 1)
    }

    pub fn new_label(&mut self) -> Label {
        self.labels += 1;
        Label(self.labels - 1)
    }

    pub fn var_ty(&self, var: Var) -> &Ty {
        &self.vars[var.0 as usize]
    }

    /// The parameter and result types, and the row.
    pub fn sig(&self) -> Sig {
        Sig {
            row: self.row.clone(),
            params: self
                .params
                .iter()
                .map(|&p| self.var_ty(p).clone())
                .collect(),
            results: self.results.clone(),
        }
    }
}

/// A function's mutability parameters, which its types use as
/// [`Mutability::Param`] 1 to `count`, and what it assumes about them.
/// Each call and closure of the function gives one argument per parameter:
/// `Imm`, `Mut` or a parameter of the caller. So a parameter is always
/// `Imm` or `Mut`, never `ReadOnly`, and a function can make objects of a
/// parameter's mutability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct MutParams {
    pub count: u32,
    /// `(a, b)` assumes that `a` is at most `b` (see
    /// [`Mutability::at_most`]): an object of mutability `b` can hold
    /// one of `a`. Each is `Imm`, `Mut`, `ReadOnly` or a parameter. A call
    /// must meet them with its arguments.
    pub assumes: Vec<(Mutability, Mutability)>,
}

/// An ADT. A tuple or a struct has one constructor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DataDecl {
    /// How many mutability parameters the field types can use, as
    /// [`Mutability::Param`](crate::Mutability::Param) 1 to `params`.
    pub params: u32,
    pub ctors: Vec<CtorDecl>,
    /// A name to show it by. See [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}

/// A host function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Extern {
    /// The module the host provides it in.
    pub module: Box<str>,
    pub name: Box<str>,
    pub sig: Sig,
    /// A name to show it by in Conduit, which needn't be the host's. See
    /// [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}

/// An effect: the signature of each of its operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectDecl {
    pub ops: Vec<Sig>,
    /// A name to show it by. See [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}

/// A deep handler of an effect.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandlerDecl {
    pub effect: EffectId,
    /// The values the clauses share, like a closure's captures.
    pub env: Vec<Ty>,
    /// Clause `i` handles operation `i`: `fn(env.., op params.., k) -> R`.
    pub ops: Vec<FuncId>,
    /// The return clause: `fn(env.., body results..) -> R`.
    pub ret: FuncId,
    /// The effects its clauses may perform, which a `handle` of it
    /// performs instead of the handled effect.
    pub row: Row,
    /// A name to show it by. See [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}
