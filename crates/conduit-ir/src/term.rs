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

impl Atom {
    /// The variable, if it is one.
    pub fn var(&self) -> Option<Var> {
        match self {
            Atom::Var(v) => Some(*v),
            Atom::Lit(_) => None,
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
///
/// Cloning, comparing, hashing and printing it for debugging recurse
/// through nested blocks, on more stack as they need it (see
/// [`deep`](crate::deep)).
#[derive(Eq)]
pub struct Block {
    pub lets: Vec<Let>,
    pub tail: Tail,
}

impl Clone for Block {
    fn clone(&self) -> Block {
        crate::deep(|| Block {
            lets: self.lets.clone(),
            tail: self.tail.clone(),
        })
    }
}

impl PartialEq for Block {
    fn eq(&self, other: &Block) -> bool {
        crate::deep(|| self.lets == other.lets && self.tail == other.tail)
    }
}

impl std::hash::Hash for Block {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        crate::deep(|| {
            self.lets.hash(state);
            self.tail.hash(state);
        })
    }
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        crate::deep(|| {
            f.debug_struct("Block")
                .field("lets", &self.lets)
                .field("tail", &self.tail)
                .finish()
        })
    }
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

    /// Every join definition in this block and the blocks nested in it.
    pub fn defs(&self) -> impl Iterator<Item = &JoinDef> {
        self.blocks().flat_map(|b| match &b.tail {
            Tail::Join { defs, .. } => &defs[..],
            _ => &[],
        })
    }

    /// Calls `visit` on this block and every block nested in it, to change
    /// them, without recursion: a block before the blocks it ends in, which
    /// it may change first.
    pub fn for_each_mut(&mut self, mut visit: impl FnMut(&mut Block)) {
        let mut todo = vec![self];
        while let Some(b) = todo.pop() {
            visit(b);
            todo.extend(b.children_mut());
        }
    }

    /// The operands of this block's own `let`s and tail, not those of the
    /// blocks nested in it.
    pub fn atoms(&self) -> impl Iterator<Item = &Atom> {
        (self.lets.iter().flat_map(|l| &l.args)).chain(self.tail.atoms())
    }

    /// [`atoms`](Self::atoms), to change.
    pub fn atoms_mut(&mut self) -> impl Iterator<Item = &mut Atom> {
        (self.lets.iter_mut().flat_map(|l| &mut l.args)).chain(self.tail.atoms_mut())
    }

    /// How big this block is: its `let`s and tails, nested ones included.
    pub fn size(&self) -> usize {
        self.blocks().map(|b| b.lets.len() + 1).sum()
    }

    /// Whether [`size`](Self::size) is over `n`, found without looking
    /// further than that.
    pub fn is_bigger_than(&self, n: usize) -> bool {
        let mut size = 0;
        self.blocks().any(|b| {
            size += b.lets.len() + 1;
            size > n
        })
    }
}

/// Drops `b` without recursing as deep as its blocks nest, which dropping
/// it as Rust does would, and which can't take more stack as it goes the
/// way [`deep`](crate::deep) does: its nested blocks are taken out first.
pub(crate) fn drop_block(b: Block) {
    let mut todo = vec![b];
    while let Some(mut b) = todo.pop() {
        match std::mem::replace(&mut b.tail, Tail::Unreachable) {
            Tail::Join { defs, body, .. } => {
                todo.extend(defs.into_iter().map(|d| d.body));
                todo.push(*body);
            }
            Tail::Case { arms, default, .. } => {
                todo.extend(arms.into_iter().map(|a| a.body));
                todo.extend(default.map(|d| *d));
            }
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
        }
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
        let tys: Vec<Ty> = self.args.iter().map(|a| f.atom_ty(a)).collect();
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

impl Tail {
    /// The operands of the tail itself: a jump's arguments, or what a
    /// `case` is on.
    pub fn atoms(&self) -> &[Atom] {
        match self {
            Tail::Jump { args, .. } => args,
            Tail::Case { scrut, .. } => std::slice::from_ref(scrut),
            Tail::Join { .. } | Tail::Unreachable | Tail::Trap => &[],
        }
    }

    /// [`atoms`](Self::atoms), to change.
    pub fn atoms_mut(&mut self) -> &mut [Atom] {
        match self {
            Tail::Jump { args, .. } => args,
            Tail::Case { scrut, .. } => std::slice::from_mut(scrut),
            Tail::Join { .. } | Tail::Unreachable | Tail::Trap => &mut [],
        }
    }
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
