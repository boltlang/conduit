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
        }
    }

    /// Whether a value of this type can be used where `other` is expected.
    /// Only an object's own mutability can differ: `Imm` and `Mut` both fit
    /// `ReadOnly`, and the row of a closure or continuation: a smaller one
    /// fits a bigger one. A data type's arguments, mutabilities and rows,
    /// must be equal, and so must the types inside a closure or
    /// continuation type.
    pub fn is_subtype(&self, other: &Ty) -> bool {
        match (self, other) {
            (Ty::Data(d1, m1, a1, r1), Ty::Data(d2, m2, a2, r2)) => {
                d1 == d2 && a1 == a2 && r1 == r2 && m1.is_sub(*m2)
            }
            (Ty::Array(e1, m1), Ty::Array(e2, m2)) => e1 == e2 && m1.is_sub(*m2),
            (Ty::Fn(s1, m1), Ty::Fn(s2, m2)) => s1.is_sub(s2) && m1.is_sub(*m2),
            (Ty::Cont(s1), Ty::Cont(s2)) => s1.is_sub(s2),
            (Ty::Any(m1), Ty::Any(m2)) => m1.is_sub(*m2),
            _ => self == other,
        }
    }

    /// This type with mutability `m`, or the same type if it has no
    /// mutability of its own (scalars, `Cont`, `Ref`).
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

    /// Whether a closure or continuation of this signature can be used
    /// where one of `other` is expected: the same types, and a row that's
    /// in `other`'s.
    pub fn is_sub(&self, other: &Sig) -> bool {
        self.params == other.params && self.results == other.results && self.row.is_in(&other.row)
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::I8 => write!(f, "i8"),
            Ty::I16 => write!(f, "i16"),
            Ty::I32 => write!(f, "i32"),
            Ty::I64 => write!(f, "i64"),
            Ty::F32 => write!(f, "f32"),
            Ty::F64 => write!(f, "f64"),
            Ty::Data(d, m, args, rows) if args.is_empty() && rows.is_empty() => {
                write!(f, "{m} {d}")
            }
            Ty::Data(d, m, args, rows) => {
                let args: Vec<String> = args
                    .iter()
                    .map(Mutability::to_string)
                    .chain(rows.iter().map(Row::to_string))
                    .collect();
                write!(f, "{m} {d}[{}]", args.join(", "))
            }
            Ty::Array(elem, m) => write!(f, "{m} [{elem}]"),
            Ty::Fn(sig, m) => write!(f, "{m} fn{sig}"),
            Ty::Cont(sig) => write!(f, "cont{sig}"),
            Ty::Ref(ty) => write!(f, "ref {ty}"),
            Ty::Any(m) => write!(f, "{m} any"),
            Ty::Reuse(c) => write!(f, "reuse {c}"),
        }
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

impl fmt::Display for Sig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}) -> ({})", list(&self.params), list(&self.results))?;
        if !self.row.is_nothing() {
            write!(f, " does {}", self.row)?;
        }
        Ok(())
    }
}

fn list(tys: &[Ty]) -> String {
    tys.iter().map(Ty::to_string).collect::<Vec<_>>().join(", ")
}

/// A constructor of an ADT.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CtorDecl {
    /// A field's type may use [`Mutability::Inherit`], for the object's
    /// mutability.
    pub fields: Vec<Ty>,
}
