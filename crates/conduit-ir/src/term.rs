use crate::id::{CtorId, Label, SourceId, Var};
use crate::op::Op;
use crate::ty::Ty;

/// An operand: a variable or a constant, never a nested expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Atom {
    Var(Var),
    Lit(Lit),
}

/// A constant. Floats are stored as their bits, so that `Lit` is `Eq` and
/// `Hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lit {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

impl Lit {
    pub fn f32(x: f32) -> Lit {
        Lit::F32(x.to_bits())
    }

    pub fn f64(x: f64) -> Lit {
        Lit::F64(x.to_bits())
    }

    pub fn ty(&self) -> Ty {
        match self {
            Lit::I32(_) => Ty::I32,
            Lit::I64(_) => Ty::I64,
            Lit::F32(_) => Ty::F32,
            Lit::F64(_) => Ty::F64,
        }
    }
}

impl std::fmt::Display for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Atom::Var(v) => write!(f, "{v}"),
            Atom::Lit(Lit::I32(n)) => write!(f, "{n}i32"),
            Atom::Lit(Lit::I64(n)) => write!(f, "{n}i64"),
            Atom::Lit(Lit::F32(bits)) => write!(f, "{}f32", f32::from_bits(*bits)),
            Atom::Lit(Lit::F64(bits)) => write!(f, "{}f64", f64::from_bits(*bits)),
        }
    }
}

/// A term. Terms only appear in tail position, so every [`Term::Jump`] is a
/// tail jump by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Term {
    /// `let x1, .., xn = op(a1, .., am); body`
    Let {
        binds: Vec<Var>,
        op: Op,
        args: Vec<Atom>,
        body: Box<Term>,
        source: Option<SourceId>,
    },

    /// `join [rec] j1(params) = t1, .., jn(params) = tn in body`
    ///
    /// The labels are in scope in `body`, and if `rec`, in every
    /// definition's body too.
    Join {
        rec: bool,
        defs: Vec<JoinDef>,
        body: Box<Term>,
    },

    /// `jump j(a1, .., an)`. A jump to [`Label::RET`] returns.
    Jump {
        target: Label,
        args: Vec<Atom>,
        source: Option<SourceId>,
    },

    /// Branch on an integer or on the constructor of an ADT value.
    /// `default` runs when no arm's tag matches.
    Case {
        scrut: Atom,
        arms: Vec<Arm>,
        default: Option<Box<Term>>,
    },

    /// This point is never reached.
    Unreachable,
}

/// One definition of a [`Term::Join`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JoinDef {
    pub label: Label,
    pub params: Vec<Var>,
    pub body: Term,
}

/// One arm of a [`Term::Case`]. An arm binds nothing: fields are read with
/// [`Op::Get`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Arm {
    pub tag: Tag,
    pub body: Term,
}

/// What a [`Term::Case`] arm matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tag {
    Int(i64),
    Ctor(CtorId),
}
