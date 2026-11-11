use std::fmt;

use crate::id::{CtorId, DataId};
use crate::row::Row;

/// A type.
///
/// Types are compared structurally, so the derived `Eq` is type equality.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    /// An 8-bit integer. Like every integer type, it has no sign: the ops
    /// say how to read it.
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    /// A GC'd ADT value: its own mutability, then one argument for each
    /// mutability parameter its declaration has, then one row for each
    /// effect parameter.
    Data(DataId, Mutability, Vec<Mutability>, Vec<Row>),
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
    /// A value of the host, like a JavaScript object in Wasm, which Conduit
    /// can't look into: it only holds it and gives it to externs. It's an
    /// object, so it's counted, and its host can let go of it once nothing
    /// refers to it. It counts as `Imm`, since nothing Conduit does can
    /// change it, though the host's own code may. It's never null.
    Host,
    /// A host value, or null, like what a JavaScript function that may give
    /// `null` gives. A `Host` fits where one is expected. `HostIsNull` tells
    /// which it is, and `HostUnwrap` gives the `Host`. A null isn't an
    /// object, so it isn't counted.
    NullableHost,
    /// A reuse token, from `DropReuse` of an object built by this
    /// constructor: its memory, if it was unique, or none. Only `Reuse` of
    /// a constructor of the same shape takes it. It isn't an object, so it
    /// isn't counted, and only variables have it.
    Reuse(CtorId),
}

impl Ty {
    /// A data type whose declaration has no parameters.
    pub fn data(d: DataId, m: Mutability) -> Ty {
        Ty::Data(d, m, Vec::new(), Vec::new())
    }

    /// Whether this is a scalar: an integer or a float.
    pub fn is_scalar(&self) -> bool {
        matches!(
            self,
            Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64
        )
    }

    /// Whether values of this type are objects, which have a reference
    /// count: anything but a scalar or a reuse token.
    pub fn is_object(&self) -> bool {
        !self.is_scalar() && !matches!(self, Ty::Reuse(_))
    }

    /// Whether this is a host value, `Host` or `NullableHost`.
    pub fn is_host(&self) -> bool {
        matches!(self, Ty::Host | Ty::NullableHost)
    }

    /// Whether this is an integer type: `i8`, `i16`, `i32` or `i64`.
    pub fn is_int(&self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64)
    }

    /// The mutability of an object type; `None` for scalars and `Cont`.
    pub fn mutability(&self) -> Option<Mutability> {
        match self {
            Ty::I8
            | Ty::I16
            | Ty::I32
            | Ty::I64
            | Ty::F32
            | Ty::F64
            | Ty::Cont(_)
            | Ty::Reuse(_) => None,
            Ty::Data(_, m, ..) | Ty::Array(_, m) | Ty::Fn(_, m) | Ty::Any(m) => Some(*m),
            Ty::Ref(_) => Some(Mutability::Mut),
            Ty::Host | Ty::NullableHost => Some(Mutability::Imm),
        }
    }

    /// This type with mutability `m`, or the same type if it has no
    /// mutability of its own (scalars, `Cont`, `Ref`, `Host`).
    pub fn with_mutability(&self, m: Mutability) -> Ty {
        match self {
            Ty::Data(d, _, args, rows) => Ty::Data(*d, m, args.clone(), rows.clone()),
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
    /// Only in a [`CtorDecl`]'s field types and in an array's element
    /// type: the mutability of the data object or array holding the field
    /// or element. It's parameter 0 of every data type and array.
    Inherit,
    /// A mutability parameter `i`, counted from 1: in a [`CtorDecl`]'s
    /// field types, the declaration's, which each data type of it gives as
    /// an argument; in a function's types, the function's, which each call
    /// and closure of it gives (see [`MutParams`](crate::MutParams)).
    Param(u32),
}

impl Mutability {
    /// Whether a value of mutability `self` can be passed where `other` is
    /// expected: `Imm ≤ ReadOnly` and `Mut ≤ ReadOnly`.
    pub fn is_sub(self, other: Mutability) -> bool {
        self == other || other == Mutability::ReadOnly
    }

    /// Whether `self` is at most `other` in the order of what can hold
    /// what, `Imm ⊑ ReadOnly ⊑ Mut`, given what a function assumes:
    /// `(a, b)` in `assumes` says that `a ⊑ b`. A parameter is `Imm` or
    /// `Mut`, so it's below `Mut` and above `Imm`, and below anything else
    /// only by what's assumed.
    pub fn at_most(self, other: Mutability, assumes: &[(Mutability, Mutability)]) -> bool {
        let known = |a: Mutability| a == other || a == Mutability::Imm || other == Mutability::Mut;
        // Follow the assumptions from `self`, each at most once.
        let mut todo = vec![self];
        let mut seen = vec![self];
        while let Some(a) = todo.pop() {
            if known(a) {
                return true;
            }
            for &(x, y) in assumes {
                if x == a && !seen.contains(&y) {
                    seen.push(y);
                    todo.push(y);
                }
            }
        }
        false
    }
}

/// A signature: parameter and result types, and the effects it may
/// perform. Either list can be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Sig {
    pub params: Vec<Ty>,
    pub results: Vec<Ty>,
    pub row: Row,
}

impl Sig {
    /// A signature with no effects.
    pub fn new(params: Vec<Ty>, results: Vec<Ty>) -> Sig {
        Sig {
            params,
            results,
            row: Row::NOTHING,
        }
    }
}

/// As in the text format.
impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::text::print::ty_text(self))
    }
}

impl fmt::Display for Mutability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mutability::Imm => f.write_str("imm"),
            Mutability::Mut => f.write_str("mut"),
            Mutability::ReadOnly => f.write_str("ro"),
            Mutability::Inherit => f.write_str("inherit"),
            Mutability::Param(i) => write!(f, "'m{i}"),
        }
    }
}

/// As in the text format: `(i32) -> (i64) does effect20`.
impl fmt::Display for Sig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::text::print::sig_text(self))
    }
}

/// Types apart by commas, as in a signature: `i32, mut data10`.
pub(crate) fn list(tys: &[Ty]) -> String {
    tys.iter().map(Ty::to_string).collect::<Vec<_>>().join(", ")
}

/// A constructor of an ADT.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CtorDecl {
    /// A field's type may use [`Mutability::Inherit`], for the object's
    /// mutability.
    pub fields: Vec<Ty>,
}
