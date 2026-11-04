//! Local simplifications, each from what a variable is known to be bound
//! to:
//!
//! - **Constant folding.** A [`Prim`] op on literals is its result, as the
//!   evaluator computes it, unless it traps, or a float in it is NaN, whose
//!   bits Wasm doesn't fix. An integer op with a literal that leaves the
//!   other argument as it is, like `add(x, 0)` or `mul(x, 1)`, is that
//!   argument, and `mul(x, 0)` and `and(x, 0)` are `0`.
//! - **Case of a known value.** A `case` on a literal, or on an object made
//!   by `new` in this function, is the arm it takes. A constructor never
//!   changes, so this holds for `Mut` objects too.
//! - **Fields of a known object.** A `get` of an `Imm` object made by
//!   `new` in this function is the `new`'s argument. A `Mut` object's
//!   field may have been set in between.
//!
//! These go together since each gives the others more to do: a folded
//! comparison picks a `case` arm, whose ops may then fold, and a field
//! read may give a literal.
//!
//! It works forward in one pass: variables are SSA, so a variable's binding
//! is seen before its uses. A `let` that's simplified away leaves its
//! variables unbound, and the arms a `case` doesn't take are dropped.

use std::collections::HashMap;

use conduit_ir::{
    Arm, Atom, Block, CtorId, Func, FuncId, JoinDef, Let, Lit, Mutability, Op, Prim, Program, Tag,
    Tail, Ty, Var,
};

use crate::Pass;

/// The simplification pass.
pub struct Simplify;

impl Pass for Simplify {
    fn name(&self) -> &'static str {
        "simplify"
    }

    fn run(&self, _: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        simplify(f)
    }
}

/// Simplifies `f`. Returns whether anything changed.
pub fn simplify(f: &mut Func) -> bool {
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    let mut cx = Cx {
        vars: &f.vars,
        subst: HashMap::new(),
        news: HashMap::new(),
        changed: false,
    };
    let body = cx.block(body);
    let changed = cx.changed;
    f.body = body;
    changed
}

struct Cx<'f> {
    vars: &'f [Ty],
    /// Variables whose `let` was simplified away, and what they are.
    subst: HashMap<Var, Atom>,
    /// The objects made by `new`: their constructor and fields.
    news: HashMap<Var, (CtorId, Vec<Atom>)>,
    changed: bool,
}

impl Cx<'_> {
    fn atom(&self, a: &mut Atom) {
        if let Atom::Var(v) = a
            && let Some(to) = self.subst.get(v)
        {
            *a = to.clone();
        }
    }

    fn block(&mut self, b: Block) -> Block {
        let mut lets = Vec::with_capacity(b.lets.len());
        let mut next = b;
        loop {
            for l in next.lets {
                self.let_(l, &mut lets);
            }
            match self.tail(next.tail) {
                Ok(tail) => return Block { lets, tail },
                // The arm a `case` takes: its `let`s go after these, and
                // its tail is this block's.
                Err(arm) => {
                    self.changed = true;
                    next = arm;
                }
            }
        }
    }

    fn let_(&mut self, mut l: Let, lets: &mut Vec<Let>) {
        l.args.iter_mut().for_each(|a| self.atom(a));
        if let [bind] = l.binds[..]
            && let Some(to) = self.value(&l)
        {
            self.subst.insert(bind, to);
            self.changed = true;
            return;
        }
        if let (Op::New(c), [v]) = (&l.op, &l.binds[..]) {
            self.news.insert(*v, (*c, l.args.clone()));
        }
        lets.push(l);
    }

    /// What the single result of `l` is, if it's known without running it.
    fn value(&self, l: &Let) -> Option<Atom> {
        match &l.op {
            Op::Prim(p) => fold(p, &l.args),
            Op::Get(c, i) => {
                let Some(Atom::Var(x)) = l.args.first() else {
                    return None;
                };
                let (made, args) = self.news.get(x)?;
                let imm = matches!(self.vars[x.0 as usize], Ty::Data(_, Mutability::Imm, _));
                (imm && made == c).then(|| args[*i as usize].clone())
            }
            _ => None,
        }
    }

    /// `t` simplified, or `Err` with the arm it takes if it's a `case` of
    /// a known value.
    fn tail(&mut self, t: Tail) -> Result<Tail, Block> {
        Ok(match t {
            Tail::Jump {
                target,
                mut args,
                source,
            } => {
                args.iter_mut().for_each(|a| self.atom(a));
                Tail::Jump {
                    target,
                    args,
                    source,
                }
            }
            Tail::Case {
                mut scrut,
                arms,
                default,
            } => {
                self.atom(&mut scrut);
                if let Some(tag) = self.tag(&scrut) {
                    let taken = arms.iter().position(|a| a.tag == tag);
                    match taken {
                        Some(i) => return Err(arms.into_iter().nth(i).unwrap().body),
                        None => {
                            if let Some(d) = default {
                                return Err(*d);
                            }
                        }
                    }
                }
                Tail::Case {
                    scrut,
                    arms: arms
                        .into_iter()
                        .map(|a| Arm {
                            tag: a.tag,
                            body: self.block(a.body),
                        })
                        .collect(),
                    default: default.map(|d| Box::new(self.block(*d))),
                }
            }
            Tail::Join { rec, defs, body } => Tail::Join {
                rec,
                defs: defs
                    .into_iter()
                    .map(|d| JoinDef {
                        body: self.block(d.body),
                        ..d
                    })
                    .collect(),
                body: Box::new(self.block(*body)),
            },
            Tail::Unreachable => Tail::Unreachable,
        })
    }

    /// The tag a `case` on `scrut` matches, if it's known.
    fn tag(&self, scrut: &Atom) -> Option<Tag> {
        match scrut {
            Atom::Lit(Lit::I8(n)) => Some(Tag::Int((*n).into())),
            Atom::Lit(Lit::I16(n)) => Some(Tag::Int((*n).into())),
            Atom::Lit(Lit::I32(n)) => Some(Tag::Int((*n).into())),
            Atom::Lit(Lit::I64(n)) => Some(Tag::Int(*n)),
            Atom::Lit(Lit::F32(_) | Lit::F64(_)) => None,
            Atom::Var(v) => self.news.get(v).map(|(c, _)| Tag::Ctor(*c)),
        }
    }
}

/// What `p` gives on `args`, if that's known.
fn fold(p: &Prim, args: &[Atom]) -> Option<Atom> {
    let lits: Option<Vec<Lit>> = args
        .iter()
        .map(|a| match a {
            Atom::Lit(l) => Some(*l),
            Atom::Var(_) => None,
        })
        .collect();
    match lits {
        Some(lits) if !lits.iter().any(is_nan) => {
            let r = conduit_eval::fold(p, &lits)?;
            (!is_nan(&r)).then_some(Atom::Lit(r))
        }
        Some(_) => None,
        None => identity(p, args),
    }
}

fn is_nan(l: &Lit) -> bool {
    match *l {
        Lit::F32(bits) => f32::from_bits(bits).is_nan(),
        Lit::F64(bits) => f64::from_bits(bits).is_nan(),
        _ => false,
    }
}

/// The integer value of `a`, if it's an integer literal.
fn int(a: &Atom) -> Option<i64> {
    match *a {
        Atom::Lit(Lit::I8(n)) => Some(n.into()),
        Atom::Lit(Lit::I16(n)) => Some(n.into()),
        Atom::Lit(Lit::I32(n)) => Some(n.into()),
        Atom::Lit(Lit::I64(n)) => Some(n),
        _ => None,
    }
}

/// An integer op with one literal argument whose result is the other
/// argument, or a literal. Floats are left alone: `x + 0.0` isn't `x` when
/// `x` is `-0.0`.
fn identity(p: &Prim, args: &[Atom]) -> Option<Atom> {
    let [x, y] = args else { return None };
    match (p, int(x), int(y)) {
        (Prim::Add | Prim::Or | Prim::Xor, Some(0), _) => Some(y.clone()),
        (Prim::Mul, Some(1), _) | (Prim::And, Some(-1), _) => Some(y.clone()),
        (Prim::Mul | Prim::And, Some(0), _) => Some(x.clone()),
        (Prim::Add | Prim::Sub | Prim::Or | Prim::Xor | Prim::Shl | Prim::Shr(_), _, Some(0)) => {
            Some(x.clone())
        }
        (Prim::Mul, _, Some(1)) | (Prim::And, _, Some(-1)) => Some(x.clone()),
        (Prim::Mul | Prim::And, _, Some(0)) => Some(y.clone()),
        _ => None,
    }
}
