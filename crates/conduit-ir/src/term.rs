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
    I8(i8),
    I16(i16),
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
            Lit::I8(_) => Ty::I8,
            Lit::I16(_) => Ty::I16,
            Lit::I32(_) => Ty::I32,
            Lit::I64(_) => Ty::I64,
            Lit::F32(_) => Ty::F32,
            Lit::F64(_) => Ty::F64,
        }
    }
}

/// As in the text format.
impl std::fmt::Display for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::text::atom_text(self))
    }
}

/// Straight-line code: `let`s, then the tail that ends it.
///
/// The `let`s are a list rather than nested, so that a long function is a
/// long `Vec`: dropping, cloning, comparing or walking it doesn't recurse.
/// Blocks only nest through `join` and `case`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Block {
    pub lets: Vec<Let>,
    pub tail: Tail,
}

impl Block {
    /// This block with `l` run first.
    pub fn with_let(mut self, l: Let) -> Block {
        self.lets.insert(0, l);
        self
    }

    /// The blocks this one ends in, directly: a `join`'s definitions, then
    /// its body, or a `case`'s arms, then its default.
    pub fn children(&self) -> impl Iterator<Item = &Block> {
        let (defs, body, arms, default) = match &self.tail {
            Tail::Join { defs, body, .. } => (&defs[..], Some(&**body), &[][..], None),
            Tail::Case { arms, default, .. } => (&[][..], None, &arms[..], default.as_deref()),
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => (&[][..], None, &[][..], None),
        };
        (defs.iter().map(|d| &d.body))
            .chain(body)
            .chain(arms.iter().map(|a| &a.body))
            .chain(default)
    }

    /// [`children`](Self::children), to change.
    pub fn children_mut(&mut self) -> impl Iterator<Item = &mut Block> {
        let (defs, body, arms, default) = match &mut self.tail {
            Tail::Join { defs, body, .. } => (&mut defs[..], Some(&mut **body), &mut [][..], None),
            Tail::Case { arms, default, .. } => {
                (&mut [][..], None, &mut arms[..], default.as_deref_mut())
            }
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {
                (&mut [][..], None, &mut [][..], None)
            }
        };
        (defs.iter_mut().map(|d| &mut d.body))
            .chain(body)
            .chain(arms.iter_mut().map(|a| &mut a.body))
            .chain(default)
    }

    /// This block and every block nested in it, without recursion: a block
    /// comes before the blocks it ends in, which come last to first.
    pub fn blocks(&self) -> Blocks<'_> {
        Blocks { todo: vec![self] }
    }

    /// Calls `visit` on each of [`blocks`](Self::blocks).
    pub fn walk<'a>(&'a self, mut visit: impl FnMut(&'a Block)) {
        self.blocks().for_each(&mut visit);
    }

    /// How big this block is: its `let`s and tails, nested ones included.
    pub fn size(&self) -> usize {
        self.blocks().map(|b| b.lets.len() + 1).sum()
    }
}

/// The blocks of [`Block::blocks`].
pub struct Blocks<'a> {
    todo: Vec<&'a Block>,
}

impl<'a> Iterator for Blocks<'a> {
    type Item = &'a Block;

    fn next(&mut self) -> Option<&'a Block> {
        let b = self.todo.pop()?;
        self.todo.extend(b.children());
        Some(b)
    }
}

/// A block without `let`s.
impl From<Tail> for Block {
    fn from(tail: Tail) -> Block {
        Block {
            lets: Vec::new(),
            tail,
        }
    }
}

/// `let x1, .., xn = op(a1, .., am)`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Let {
    pub binds: Vec<Var>,
    pub op: Op,
    pub args: Vec<Atom>,
    pub source: Option<SourceId>,
}

impl Let {
    /// The effects this `let` of `f` may perform: its op's
    /// ([`Op::row`]), with the types of its arguments.
    pub fn row<P: crate::Program + ?Sized>(&self, p: &P, f: &crate::Func) -> crate::Row {
        let tys: Vec<Ty> = (self.args.iter())
            .map(|a| match a {
                Atom::Var(v) => f.var_ty(*v).clone(),
                Atom::Lit(lit) => lit.ty(),
            })
            .collect();
        self.op.row(p, &tys)
    }
}

/// What ends a block. A block's tail is its only exit, so every
/// [`Tail::Jump`] is a tail jump by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Tail {
    /// `join [rec] j1(params) = b1, .., jn(params) = bn in body`
    ///
    /// The labels are in scope in `body`, and if `rec`, in every
    /// definition's body too.
    Join {
        rec: bool,
        defs: Vec<JoinDef>,
        body: Box<Block>,
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
        default: Option<Box<Block>>,
    },

    /// This point is never reached: the frontend promises it, so reaching
    /// it is undefined behavior, and passes may assume it doesn't happen,
    /// like after a call of a function that never returns, or in an arm
    /// its type checker shows can't be taken. Compare [`Tail::Trap`].
    Unreachable,

    /// Stops the program with an error, like an `abort()`. Unlike
    /// [`Tail::Unreachable`], reaching it is defined, so passes keep it.
    Trap,
}

/// One definition of a [`Tail::Join`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JoinDef {
    pub label: Label,
    pub params: Vec<Var>,
    pub body: Block,
}

/// One arm of a [`Tail::Case`]. An arm binds nothing: fields are read with
/// [`Op::Get`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Arm {
    pub tag: Tag,
    pub body: Block,
}

/// What a [`Tail::Case`] arm matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tag {
    Int(i64),
    Ctor(CtorId),
}
