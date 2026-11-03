use std::collections::BTreeMap;

use crate::id::{DataId, EffectId, ExternId, FuncId, HandlerId, Label, Var};
use crate::term::{Block, Tail};
use crate::ty::{CtorDecl, Sig, Ty};

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
    /// The type of every variable, indexed by [`Var`].
    pub vars: Vec<Ty>,
    /// How many labels the body uses, [`Label::RET`] included.
    pub labels: u32,
    pub params: Vec<Var>,
    pub results: Vec<Ty>,
    pub body: Block,
}

impl Func {
    /// A function with the given parameter types and nothing else, so its
    /// body is `unreachable` until it's set.
    pub fn new(params: Vec<Ty>, results: Vec<Ty>) -> Func {
        let params_len = params.len() as u32;
        Func {
            vars: params,
            labels: 1,
            params: (0..params_len).map(Var).collect(),
            results,
            body: Tail::Unreachable.into(),
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

    /// The parameter and result types.
    pub fn sig(&self) -> Sig {
        Sig {
            params: self
                .params
                .iter()
                .map(|&p| self.var_ty(p).clone())
                .collect(),
            results: self.results.clone(),
        }
    }
}

/// An ADT. A tuple or a struct has one constructor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DataDecl {
    pub ctors: Vec<CtorDecl>,
}

/// A host function.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Extern {
    /// The module the host provides it in.
    pub module: Box<str>,
    pub name: Box<str>,
    pub sig: Sig,
}

/// An effect: the signature of each of its operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectDecl {
    pub ops: Vec<Sig>,
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
}
