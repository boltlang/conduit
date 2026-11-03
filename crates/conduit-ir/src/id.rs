//! Identifiers.
//!
//! Items of a module (functions, data types, externs, effects, handlers) have
//! opaque `u64` IDs that the frontend chooses. They must stay the same for
//! the same item across compilations, so that an unchanged function has
//! unchanged IR. Positions only exist after linking.
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

impl Label {
    /// The label in scope in every function body, whose parameters are the
    /// function's results. Returning is `jump ret(..)`.
    pub const RET: Label = Label(0);
}
