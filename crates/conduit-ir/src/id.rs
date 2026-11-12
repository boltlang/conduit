//! Identifiers.
//!
//! Items of a module (functions, data types, externs, effects, handlers,
//! constants) have opaque `u64` IDs that the frontend chooses. They must
//! stay the same for the same item across compilations, so that an
//! unchanged function has unchanged IR. Positions only exist after linking.
//!
//! Variables and labels are local to a function: indices into its tables.

macro_rules! opaque_id {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);
    )*};
}

opaque_id! {
    /// A top-level function.
    FuncId,
    /// An ADT.
    DataId,
    /// A host function.
    ExternId,
    /// An effect: a set of operations.
    EffectId,
    /// A handler of an effect.
    HandlerId,
    /// A constant: an `Imm` value made once, before the program runs.
    ConstId,
    /// Where code comes from, for errors. Conduit never looks inside it.
    SourceId,
}

/// A constructor: its ADT and its index among the ADT's constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CtorId {
    pub data: DataId,
    pub index: u32,
}

/// A variable: an index into [`Func::vars`](crate::Func::vars).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(pub u32);

/// A join point. Labels are numbered per function, and [`Label::RET`] is the
/// function's return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(pub u32);

impl EffectId {
    /// The built-in effect `throw`, of exceptions from the host, whose one
    /// operation, `raise`, gives the value thrown: see
    /// `docs/exceptions.md`. A module that uses it has its declaration,
    /// [`EffectDecl::throw`](crate::EffectDecl::throw), under this ID.
    pub const THROW: EffectId = EffectId(u64::MAX);
}

impl Label {
    /// The label in scope in every function body, whose parameters are the
    /// function's results. Returning is `jump ret(..)`.
    pub const RET: Label = Label(0);
}

macro_rules! display {
    ($($name:ident => $fmt:literal),* $(,)?) => {$(
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, $fmt, self.0)
            }
        }
    )*};
}

display! {
    FuncId => "fn{}",
    DataId => "data{}",
    ExternId => "extern{}",
    HandlerId => "handler{}",
    ConstId => "const{}",
    SourceId => "src{}",
    Var => "%{}",
}

/// `effect{n}`, or `throw` for [`EffectId::THROW`].
impl std::fmt::Display for EffectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if *self == EffectId::THROW {
            f.write_str("throw")
        } else {
            write!(f, "effect{}", self.0)
        }
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if *self == Label::RET {
            write!(f, "ret")
        } else {
            write!(f, "j{}", self.0)
        }
    }
}

impl std::fmt::Display for CtorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.data, self.index)
    }
}
