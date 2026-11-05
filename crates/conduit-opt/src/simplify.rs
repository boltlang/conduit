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
//! - **What an arm says.** In an arm `C =>` of a `case` on `x`, a `case`
//!   on `x` is known to be `C`, and in an arm `3 =>`, `x` is `3`.
//! - **Unboxing what was boxed.** `unbox[T]` of `box[T](x)` is `x`. At
//!   another type it's undefined, so it stays.
//! - **Arrays made here.** The length of an array made by `array_new`,
//!   `array_fill`, `array_alloc`, or `array_clone` or `array_freeze` of one
//!   whose length is known, is known: `array_fill` and `array_alloc` trap
//!   on a length they can't make. An element of an `Imm` one at a literal
//!   index in bounds is what it was made with.
//! - **Parts of a constant.** A constant that's a literal is the literal.
//!   A `case` on a constant takes its arm, as for
//!   `new`. A field of a constant, its length and an element at a literal
//!   index in bounds are known: a literal, or `const` of the constant that
//!   part is. A part that's neither, like a nested array, has no name, so
//!   reading it stays.
//!
//! These go together since each gives the others more to do: a folded
//! comparison picks a `case` arm, whose ops may then fold, and a field
//! read may give a literal.
//!
//! It works forward in one pass: variables are SSA, so a variable's binding
//! is seen before its uses. A `let` that's simplified away leaves its
//! variables unbound, and the arms a `case` doesn't take are dropped.

use std::collections::HashMap;

use conduit_ir::verify::const_value;
use conduit_ir::{
    Arm, Atom, Block, ConstId, ConstValue, CtorId, Func, FuncId, JoinDef, Let, Lit, Mutability, Op,
    Prim, Program, Tag, Tail, Ty, Var,
};

use crate::Pass;

/// The simplification pass.
pub struct Simplify;

impl Pass for Simplify {
    fn name(&self) -> &'static str {
        "simplify"
    }

    fn run(&self, p: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        simplify(p, f)
    }
}

/// Simplifies `f`, a function of `p`. Returns whether anything changed.
pub fn simplify(p: &dyn Program, f: &mut Func) -> bool {
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    let mut cx = Cx {
        p,
        consts: HashMap::new(),
        vars: &f.vars,
        subst: HashMap::new(),
        news: HashMap::new(),
        boxes: HashMap::new(),
        arrays: HashMap::new(),
        arms: Vec::new(),
        changed: false,
    };
    let body = cx.block(body);
    let changed = cx.changed;
    f.body = body;
    changed
}

struct Cx<'f> {
    p: &'f dyn Program,
    /// The variables bound by `const`, and the constant.
    consts: HashMap<Var, ConstId>,
    vars: &'f [Ty],
    /// Variables whose `let` was simplified away, and what they are.
    subst: HashMap<Var, Atom>,
    /// The objects made by `new`: their constructor and fields.
    news: HashMap<Var, (CtorId, Vec<Atom>)>,
    /// What `box` boxed, and at which type.
    boxes: HashMap<Var, (Ty, Atom)>,
    /// The arrays made here whose length is known.
    arrays: HashMap<Var, Made>,
    /// What the enclosing `case` arms say a variable was built by.
    arms: Vec<(Var, CtorId)>,
    changed: bool,
}

/// An array made here: its length, and for an `Imm` one, its elements.
#[derive(Clone)]
struct Made {
    len: Atom,
    elems: Option<Elems>,
}

#[derive(Clone)]
enum Elems {
    /// These, by index.
    All(Vec<Atom>),
    /// This one, at every index.
    Fill(Atom),
}

impl<'f> Cx<'f> {
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
        if let [_] = l.binds[..]
            && let Some(ConstValue::Const(id)) = self.const_part(&l)
        {
            l.op = Op::Const(*id);
            l.args.clear();
            self.changed = true;
        }
        match (&l.op, &l.binds[..]) {
            (Op::New(c), [v]) => {
                self.news.insert(*v, (*c, l.args.clone()));
            }
            (Op::Const(id), [v]) => {
                self.consts.insert(*v, *id);
            }
            (Op::Box(ty), [v]) => {
                self.boxes.insert(*v, (ty.clone(), l.args[0].clone()));
            }
            (op, [v]) => {
                if let Some(made) = self.made(op, &l.args, *v) {
                    self.arrays.insert(*v, made);
                }
            }
            _ => {}
        }
        lets.push(l);
    }

    /// What's known about the array `op` makes, binding `v`.
    fn made(&self, op: &Op, args: &[Atom], v: Var) -> Option<Made> {
        let imm = matches!(self.vars[v.0 as usize], Ty::Array(_, Mutability::Imm));
        Some(match op {
            Op::ArrayNew => Made {
                len: Atom::Lit(Lit::I32(args.len() as i32)),
                elems: imm.then(|| Elems::All(args.to_vec())),
            },
            Op::ArrayFill => Made {
                len: args[0].clone(),
                elems: imm.then(|| Elems::Fill(args[1].clone())),
            },
            Op::ArrayAlloc => Made {
                len: args[0].clone(),
                elems: None,
            },
            // The same length, and the same elements if they couldn't have
            // changed since the original was made.
            Op::ArrayClone | Op::ArrayFreeze => {
                let Atom::Var(src) = &args[0] else {
                    return None;
                };
                let from = self.arrays.get(src)?;
                let src_imm = matches!(self.vars[src.0 as usize], Ty::Array(_, Mutability::Imm));
                Made {
                    len: from.len.clone(),
                    elems: from.elems.clone().filter(|_| imm && src_imm),
                }
            }
            _ => return None,
        })
    }

    /// Element `i` of an array made here, if it's known.
    fn element(&self, array: Option<&Atom>, index: Option<&Atom>) -> Option<Atom> {
        let (Some(Atom::Var(x)), Some(Atom::Lit(Lit::I32(i)))) = (array, index) else {
            return None;
        };
        let made = self.arrays.get(x)?;
        let i = usize::try_from(*i).ok()?;
        match made.elems.as_ref()? {
            Elems::All(elems) => elems.get(i).cloned(),
            Elems::Fill(value) => match made.len {
                Atom::Lit(Lit::I32(len)) if i < len as usize => Some(value.clone()),
                _ => None,
            },
        }
    }

    /// What constant `a` is, if it's a variable bound by `const`.
    fn constant(&self, a: Option<&Atom>) -> Option<&'f ConstValue> {
        let Some(Atom::Var(x)) = a else { return None };
        const_value(self.p, *self.consts.get(x)?)
    }

    /// The part of a constant that `l` reads, if it's known: a field, or
    /// an element at a literal index in bounds.
    fn const_part(&self, l: &Let) -> Option<&'f ConstValue> {
        match (&l.op, self.constant(l.args.first())?) {
            (Op::Get(c, i), ConstValue::New(made, fields)) if made == c => fields.get(*i as usize),
            (Op::ArrayGet, ConstValue::Array(elems)) => match l.args.get(1) {
                Some(Atom::Lit(Lit::I32(i))) => elems.get(usize::try_from(*i).ok()?),
                _ => None,
            },
            _ => None,
        }
    }

    /// What the single result of `l` is, if it's known without running it.
    fn value(&self, l: &Let) -> Option<Atom> {
        let known = match &l.op {
            Op::Prim(p) => fold(p, &l.args),
            Op::Get(c, i) => self.new_field(*c, *i, l.args.first()),
            Op::Const(id) => match const_value(self.p, *id)? {
                ConstValue::Lit(lit) => Some(Atom::Lit(*lit)),
                _ => None,
            },
            Op::ArrayLen => match (self.constant(l.args.first()), l.args.first()) {
                (Some(ConstValue::Array(elems)), _) => {
                    Some(Atom::Lit(Lit::I32(elems.len() as i32)))
                }
                (_, Some(Atom::Var(x))) => self.arrays.get(x).map(|m| m.len.clone()),
                _ => None,
            },
            Op::ArrayGet => self.element(l.args.first(), l.args.get(1)),
            Op::Unbox(ty) => match l.args.first() {
                Some(Atom::Var(b)) => match self.boxes.get(b) {
                    Some((boxed, x)) if boxed == ty => Some(x.clone()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        known.or_else(|| match self.const_part(l)? {
            ConstValue::Lit(lit) => Some(Atom::Lit(*lit)),
            _ => None,
        })
    }

    /// Field `i` of `obj`, if it's an `Imm` object `new c` made.
    fn new_field(&self, c: CtorId, i: u32, obj: Option<&Atom>) -> Option<Atom> {
        let Some(Atom::Var(x)) = obj else {
            return None;
        };
        let (made, args) = self.news.get(x)?;
        let imm = matches!(self.vars[x.0 as usize], Ty::Data(_, Mutability::Imm, _));
        (imm && *made == c).then(|| args[i as usize].clone())
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
                let arms = arms
                    .into_iter()
                    .map(|a| Arm {
                        tag: a.tag,
                        body: self.arm(&scrut, a.tag, a.body),
                    })
                    .collect();
                Tail::Case {
                    scrut,
                    arms,
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

    /// The arm `tag` of a `case` on `scrut`, simplified knowing that it
    /// matched.
    fn arm(&mut self, scrut: &Atom, tag: Tag, body: Block) -> Block {
        let Atom::Var(x) = scrut else {
            return self.block(body);
        };
        match tag {
            Tag::Ctor(c) => {
                self.arms.push((*x, c));
                let body = self.block(body);
                self.arms.pop();
                body
            }
            Tag::Int(n) => {
                let lit = match self.vars[x.0 as usize] {
                    Ty::I8 => Lit::I8(n as i8),
                    Ty::I16 => Lit::I16(n as i16),
                    Ty::I32 => Lit::I32(n as i32),
                    Ty::I64 => Lit::I64(n),
                    _ => return self.block(body),
                };
                self.subst.insert(*x, Atom::Lit(lit));
                let body = self.block(body);
                self.subst.remove(x);
                body
            }
        }
    }

    /// The tag a `case` on `scrut` matches, if it's known.
    fn tag(&self, scrut: &Atom) -> Option<Tag> {
        match scrut {
            Atom::Lit(Lit::I8(n)) => Some(Tag::Int((*n).into())),
            Atom::Lit(Lit::I16(n)) => Some(Tag::Int((*n).into())),
            Atom::Lit(Lit::I32(n)) => Some(Tag::Int((*n).into())),
            Atom::Lit(Lit::I64(n)) => Some(Tag::Int(*n)),
            Atom::Lit(Lit::F32(_) | Lit::F64(_)) => None,
            Atom::Var(v) => {
                if let Some((c, _)) = self.news.get(v) {
                    return Some(Tag::Ctor(*c));
                }
                if let Some((_, c)) = self.arms.iter().rev().find(|(x, _)| x == v) {
                    return Some(Tag::Ctor(*c));
                }
                match self.constant(Some(scrut))? {
                    ConstValue::New(c, _) => Some(Tag::Ctor(*c)),
                    _ => None,
                }
            }
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
