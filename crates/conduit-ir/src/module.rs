use std::collections::{BTreeMap, BTreeSet};

use crate::Program;

use crate::id::{ConstId, CtorId, DataId, EffectId, ExternId, FuncId, HandlerId, Label, Var};
use crate::row::Row;
use crate::term::{Block, Lit, Tail};
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
    pub consts: BTreeMap<ConstId, ConstDecl>,
}

/// A top-level function. Nested functions don't exist: frontends lift them,
/// with their captured values as the first parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Func {
    /// The mutability parameters its types can use.
    pub mut_params: MutParams,
    /// How many effect parameters its rows can use, as `E1` to `En`. Each
    /// call and closure of it gives a row for each.
    pub effect_params: u32,
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
            effect_params: 0,
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
    /// How many effect parameters the field types' rows can use, as `E1`
    /// to `En`. Each data type of it gives a row for each.
    pub effect_params: u32,
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
    /// How a backend passes its arguments and results.
    pub abi: Abi,
    /// For each parameter, whether a [`Abi::Flat`] array is passed as its
    /// elements' address alone, without its length. Empty for none.
    pub nolen: Vec<bool>,
    /// Names to show its parameters by in Conduit, which mean nothing to
    /// the host. Empty for none.
    pub param_names: Vec<Option<Box<str>>>,
    /// A name to show it by in Conduit, which needn't be the host's. See
    /// [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}

impl Extern {
    /// A host function with the default ABI.
    pub fn new(module: &str, name: &str, sig: Sig) -> Extern {
        Extern {
            module: module.into(),
            name: name.into(),
            sig,
            abi: Abi::Conduit,
            nolen: Vec::new(),
            param_names: Vec::new(),
            debug_name: None,
        }
    }

    /// Whether parameter `i` is passed without its length.
    pub fn nolen(&self, i: usize) -> bool {
        self.nolen.get(i).copied().unwrap_or(false)
    }
}

/// How an extern takes its arguments and gives its results. The
/// evaluator ignores it: its hosts take and give values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Abi {
    /// Values as the backend represents them: an object is its address.
    #[default]
    Conduit,
    /// Addresses and lengths, as C compilers for wasm32 pass them, for WASI
    /// and other C-like hosts. Only scalars, arrays of scalars and arrays
    /// of `[i8]` (lists of byte buffers) can be passed, and only scalars
    /// given back. In Wasm:
    ///
    /// - a scalar is itself, an `i8` or `i16` zero-extended;
    /// - an array is its elements' address, then its length, or the
    ///   address alone if the parameter is `nolen`;
    /// - a list of byte buffers is the address and count of a list of
    ///   (address, length) pairs, an iovec list, made for the call;
    /// - the first result is the import's result, and each other result
    ///   is an address to write it to, after the arguments.
    ///
    /// The host may write only to `mut` arrays, and only during the call.
    Flat,
}

/// An effect: the signature of each of its operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectDecl {
    pub ops: Vec<Sig>,
    /// A name to show it by. See [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}

/// A deep handler of one or more effects. A `perform` goes to the nearest
/// handler with a clause for its operation, and the clause runs outside
/// the whole handler: what it performs goes to the handlers around it,
/// even an operation of another effect this handler handles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandlerDecl {
    /// How many effect parameters its types and row can use, as `E1` to
    /// `En`. Each `handle` of it gives a row for each, and each clause has
    /// as many, which the `handle`'s rows are given to.
    pub effect_params: u32,
    /// The values the clauses share, like a closure's captures.
    pub env: Vec<Ty>,
    /// The effects it handles, each once, with their clauses: clause `i`
    /// of an effect handles its operation `i`, as
    /// `fn(env.., op params.., k) -> R`. Every operation has one.
    pub ops: Vec<(EffectId, Vec<FuncId>)>,
    /// The return clause: `fn(env.., body results..) -> R`.
    pub ret: FuncId,
    /// The effects its clauses may perform, which a `handle` of it
    /// performs instead of the handled effects.
    pub row: Row,
    /// A name to show it by. See [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}

impl HandlerDecl {
    /// The effects it handles, in order.
    pub fn effects(&self) -> impl Iterator<Item = EffectId> + '_ {
        self.ops.iter().map(|(e, _)| *e)
    }

    /// Whether it has clauses for `e`.
    pub fn handles(&self, e: EffectId) -> bool {
        self.effects().any(|h| h == e)
    }

    /// The clause for operation `op` of `e`.
    pub fn clause(&self, e: EffectId, op: u32) -> Option<FuncId> {
        let (_, clauses) = self.ops.iter().find(|(h, _)| *h == e)?;
        clauses.get(op as usize).copied()
    }

    /// Where the clause for operation `op` of `e` is among all of them, in
    /// the order of [`clauses`](Self::clauses), as a backend numbers them.
    pub fn slot(&self, e: EffectId, op: u32) -> Option<usize> {
        let mut base = 0;
        for (h, clauses) in &self.ops {
            if *h == e {
                return ((op as usize) < clauses.len()).then_some(base + op as usize);
            }
            base += clauses.len();
        }
        None
    }

    /// Every clause, with its effect and operation, in order.
    pub fn clauses(&self) -> impl Iterator<Item = (EffectId, u32, FuncId)> + '_ {
        (self.ops.iter())
            .flat_map(|(e, clauses)| (clauses.iter().enumerate()).map(|(i, &c)| (*e, i as u32, c)))
    }

    /// How many clauses it has, not counting the return clause.
    pub fn len(&self) -> usize {
        self.ops.iter().map(|(_, clauses)| clauses.len()).sum()
    }

    /// Whether it has no clauses but the return clause.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A constant: an `Imm` value that exists before the program runs, which
/// [`Op::Const`](crate::Op::Const) gives. Every use gets the same object,
/// as an LLVM global constant would be. Since it's `Imm` all the way down,
/// nothing can tell that it's shared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstDecl {
    /// Its type, which must be `Imm` all the way down.
    pub ty: Ty,
    pub value: ConstValue,
    /// A name to show it by. See [`FuncDebug`].
    pub debug_name: Option<Box<str>>,
}

/// What a constant is made of. The types come from the [`ConstDecl`]'s:
/// an array's elements have its element type, and a constructor's fields
/// the types of the fields.
///
/// Values nest through `New` without a limit, so a long list written as
/// one value is deep, and dropping or walking it recurses. A frontend can
/// cut one up into several constants with [`ConstValue::Const`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstValue {
    Lit(Lit),
    /// An `Imm` array of these elements.
    Array(Vec<ConstValue>),
    /// An `Imm` object of this constructor, with these fields.
    New(CtorId, Vec<ConstValue>),
    /// The same object as another constant. Constants can't refer to each
    /// other in a cycle.
    Const(ConstId),
}

/// What constant `id` is, past the constants that are only another name
/// for one: `None` if one of them is unknown, or they refer to each other
/// in a cycle.
pub fn const_value<P: Program + ?Sized>(p: &P, id: ConstId) -> Option<&ConstValue> {
    let mut seen = BTreeSet::new();
    let mut value = &p.constant(id)?.value;
    while let ConstValue::Const(next) = value {
        if !seen.insert(*next) {
            return None;
        }
        value = &p.constant(*next)?.value;
    }
    Some(value)
}

/// The constants a value refers to.
pub fn const_refs(value: &ConstValue) -> Vec<ConstId> {
    let mut refs = Vec::new();
    let mut todo = vec![value];
    while let Some(v) = todo.pop() {
        match v {
            ConstValue::Lit(_) => {}
            ConstValue::Array(parts) | ConstValue::New(_, parts) => todo.extend(parts),
            ConstValue::Const(id) => refs.push(*id),
        }
    }
    refs
}
