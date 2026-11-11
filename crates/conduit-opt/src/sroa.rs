//! Scalar replacement, like LLVM's SROA: a `mut` object or array that
//! doesn't escape becomes a ref per field or element, which `promote`
//! then turns into plain values, so nothing is allocated.
//!
//! An object made by `new C(..)` qualifies when its only uses are as the
//! object of `get C.i` and `set C.i`. An array made by `array_new(..)`
//! qualifies when its only uses are as the array of `array_get` and
//! `array_set` at a literal index in bounds. Any other use keeps it: a
//! `case` on it, which `simplify` takes the arm of first, an `array_len`,
//! which `simplify` folds, or anything that lets it escape. So does a
//! field or element that's a continuation, whose ref `promote` keeps.
//!
//! Then `new C(a0, .., an)` is `ref_new(ai)` for each field, `get C.i(x)`
//! is `ref_get(ri)` and `set C.i(x, v)` is `ref_set(ri, v)`, and the same
//! for arrays.

use std::collections::{BTreeMap, BTreeSet};

use conduit_ir::types::{elem_ty, resolve};
use conduit_ir::{Atom, Block, Func, FuncId, Let, Lit, Mutability, Op, Program, Tail, Ty, Var};

use crate::Pass;

/// The scalar replacement pass.
pub struct Sroa;

impl Pass for Sroa {
    fn name(&self) -> &'static str {
        "sroa"
    }

    fn run(&self, p: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        sroa(p, f)
    }
}

/// What an object or array that may be replaced is made of.
struct Candidate {
    /// The constructor, for an object.
    ctor: Option<conduit_ir::CtorId>,
    /// The type of each field or element.
    tys: Vec<Ty>,
}

/// Replaces the objects and arrays of `f`, a function of `p`, that don't
/// escape. Returns whether anything changed.
pub fn sroa(p: &dyn Program, f: &mut Func) -> bool {
    let mut candidates: BTreeMap<Var, Candidate> = BTreeMap::new();
    for b in f.body.blocks() {
        for l in &b.lets {
            let [x] = l.binds[..] else { continue };
            let ty = f.var_ty(x);
            let candidate = match (&l.op, ty) {
                (Op::New(c), Ty::Data(d, Mutability::Mut, args, rows)) => {
                    let Some(ctor) = p.data(*d).and_then(|d| d.ctors.get(c.index as usize)) else {
                        continue;
                    };
                    let tys = ctor
                        .fields
                        .iter()
                        .map(|field| resolve(field, Mutability::Mut, args, rows))
                        .collect();
                    Candidate {
                        ctor: Some(*c),
                        tys,
                    }
                }
                (Op::ArrayNew, Ty::Array(_, Mutability::Mut)) => Candidate {
                    ctor: None,
                    tys: vec![elem_ty(ty).unwrap(); l.args.len()],
                },
                _ => continue,
            };
            if !candidate.tys.iter().any(|t| matches!(t, Ty::Cont(_))) {
                candidates.insert(x, candidate);
            }
        }
    }
    if candidates.is_empty() {
        return false;
    }

    // Every use other than as the object of an access it can replace keeps
    // a candidate, such as being the value a `set` stores.
    let mut kept = BTreeSet::new();
    for b in f.body.blocks() {
        for l in &b.lets {
            for (j, a) in l.args.iter().enumerate() {
                let Atom::Var(v) = a else { continue };
                let Some(c) = candidates.get(v) else { continue };
                if j != 0 || field(c, &l.op, &l.args).is_none() {
                    kept.insert(*v);
                }
            }
        }
        kept.extend(b.tail.atoms().iter().filter_map(Atom::var));
    }
    candidates.retain(|v, _| !kept.contains(v));
    if candidates.is_empty() {
        return false;
    }

    // A ref for each field or element, named after the object's name and
    // the field's index, like `s_0`.
    let mut refs: BTreeMap<Var, Vec<Var>> = BTreeMap::new();
    for (&x, c) in &candidates {
        let name = f.debug.vars.get(&x).cloned();
        let mut vars = Vec::new();
        for (i, t) in c.tys.iter().enumerate() {
            let r = f.new_var(Ty::Ref(Box::new(t.clone())));
            if let Some(name) = &name {
                f.debug.vars.insert(r, format!("{name}_{i}").into());
            }
            vars.push(r);
        }
        refs.insert(x, vars);
    }
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    let cx = Cx {
        candidates: &candidates,
        refs: &refs,
    };
    f.body = cx.block(body);
    true
}

/// The field or element that an access `op(args..)` of a candidate reads
/// or writes, if it's one this pass can replace.
fn field(c: &Candidate, op: &Op, args: &[Atom]) -> Option<usize> {
    match (op, c.ctor) {
        (Op::Get(made, i) | Op::Set(made, i), Some(ctor)) if *made == ctor => Some(*i as usize),
        (Op::ArrayGet | Op::ArraySet, None) => match args.get(1) {
            Some(Atom::Lit(Lit::I32(i))) => usize::try_from(*i).ok().filter(|&i| i < c.tys.len()),
            _ => None,
        },
        _ => None,
    }
}

struct Cx<'a> {
    candidates: &'a BTreeMap<Var, Candidate>,
    refs: &'a BTreeMap<Var, Vec<Var>>,
}

impl Cx<'_> {
    fn block(&self, mut b: Block) -> Block {
        conduit_ir::deep(|| {
            let mut lets = Vec::with_capacity(b.lets.len());
            for l in std::mem::take(&mut b.lets) {
                self.let_(l, &mut lets);
            }
            b.lets = lets;
            b.tail = b.tail.map_blocks(|b| self.block(b));
            b
        })
    }

    fn let_(&self, l: Let, lets: &mut Vec<Let>) {
        let made = match &l.binds[..] {
            [x] => self.refs.get(x),
            _ => None,
        };
        if let (Some(refs), Op::New(_) | Op::ArrayNew) = (made, &l.op) {
            for (&r, a) in refs.iter().zip(&l.args) {
                lets.push(Let {
                    binds: vec![r],
                    op: Op::RefNew,
                    args: vec![a.clone()],
                    source: l.source,
                });
            }
            return;
        }
        let access = match l.args.first() {
            Some(Atom::Var(x)) => self
                .candidates
                .get(x)
                .and_then(|c| field(c, &l.op, &l.args))
                .map(|i| self.refs[x][i]),
            _ => None,
        };
        let Some(r) = access else {
            lets.push(l);
            return;
        };
        let (op, args) = match l.op {
            Op::Get(..) | Op::ArrayGet => (Op::RefGet, vec![Atom::Var(r)]),
            // The value is the last argument.
            _ => (
                Op::RefSet,
                vec![Atom::Var(r), l.args.last().unwrap().clone()],
            ),
        };
        lets.push(Let {
            binds: l.binds,
            op,
            args,
            source: l.source,
        });
    }
}
