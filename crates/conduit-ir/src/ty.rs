use crate::id::DataId;

/// A type.
///
/// Types are compared structurally: there's exactly one way to write each
/// type, so the derived `Eq` is type equality.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    I32,
    I64,
    F32,
    F64,
    /// A GC'd ADT value.
    Data(DataId, Mutability),
    /// A GC'd array.
    Array(Box<Ty>, Mutability),
    /// A closure. `Imm` means it captured only what's `Imm` all the way down.
    Fn(Box<Sig>, Mutability),
    /// A one-shot continuation captured by a handler.
    Cont(Box<Sig>),
    /// A mutable cell holding one value. Always `Mut`.
    Ref(Box<Ty>),
    /// A boxed value of any type. `Imm` means it holds a value that's `Imm`
    /// all the way down.
    Any(Mutability),
}

impl Ty {
    /// The mutability of an object type; `None` for scalars and `Cont`.
    pub fn mutability(&self) -> Option<Mutability> {
        match self {
            Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 | Ty::Cont(_) => None,
            Ty::Data(_, m) | Ty::Array(_, m) | Ty::Fn(_, m) | Ty::Any(m) => Some(*m),
            Ty::Ref(_) => Some(Mutability::Mut),
        }
    }

    /// This type with mutability `m`, or the same type if it has no
    /// mutability of its own (scalars, `Cont`, `Ref`).
    pub fn with_mutability(&self, m: Mutability) -> Ty {
        match self {
            Ty::Data(d, _) => Ty::Data(*d, m),
            Ty::Array(elem, _) => Ty::Array(elem.clone(), m),
            Ty::Fn(sig, _) => Ty::Fn(sig.clone(), m),
            Ty::Any(_) => Ty::Any(m),
            other => other.clone(),
        }
    }
}

/// The mutability of an object. `Imm` is deep: nothing reachable from an
/// `Imm` value ever changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mutability {
    Imm,
    Mut,
    /// A view that accepts both `Imm` and `Mut`, and can't be written.
    ReadOnly,
    /// Only in a [`CtorDecl`]'s field types: the mutability of the object
    /// holding the field.
    Inherit,
}

impl Mutability {
    /// Whether a value of mutability `self` can be passed where `other` is
    /// expected: `Imm ≤ ReadOnly` and `Mut ≤ ReadOnly`.
    pub fn is_sub(self, other: Mutability) -> bool {
        self == other || other == Mutability::ReadOnly
    }
}

/// A signature: parameter and result types. Either list can be empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sig {
    pub params: Vec<Ty>,
    pub results: Vec<Ty>,
}

/// A constructor of an ADT.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CtorDecl {
    /// A field's type may use [`Mutability::Inherit`].
    pub fields: Vec<Ty>,
}
