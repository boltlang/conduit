//! What a frame owns while one of its `let`s runs, in counted code: the
//! references that "A continuation owns what its frames own" in
//! `docs/ir.md` is about, which dropping a continuation that's never
//! resumed drops, and which an abort that unwinds the frame drops on its
//! way out. The evaluator, the Wasm backend and native code use this one
//! analysis, so that they drop the same references.
//!
//! A frame owns what the rest of its code would have given away or
//! dropped: for each variable, the references the rest takes (see
//! [`Op::takes`], with the callees' [`Func::borrowed`] parameters), less
//! the `dup`s it makes. The counting rules make that the same on every
//! path, with one exception: where `specialize_drops` split a `drop(x)` in
//! two, a field read out of `x` is owned in the branch where `x` was
//! unique, which freed `x` and left its reference to the field with the
//! variable, and borrowed in the other. Before the split, `x` holds it, so
//! the branch that owns less is the one that's right, and a `case` owns
//! the least its branches do. A branch that only stops, `trap` or
//! `unreachable`, says nothing.
//!
//! Only counted variables own references, as `insert_counts` counts them:
//! objects, other than what `const` and `undefined` bind, and other than
//! what's lent, the borrowed parameters and what's read out of an `imm`
//! one, whose references their caller keeps.

use std::collections::{BTreeMap, BTreeSet};

use crate::tail::join_defs;
use crate::{Atom, Block, Func, FuncId, Label, Let, Module, Mutability, Op, Row, Tail, Ty, Var};

type Counts = BTreeMap<Var, i64>;

/// The variables of `f` that are lent rather than owned: its parameters
/// that `borrowed` says are borrowed, and what's read out of those by
/// `get`, `array_get` or `unbox`, if they're `imm`, whose fields can't
/// change while the caller keeps them.
pub fn lent(f: &Func, borrowed: impl Fn(usize) -> bool) -> BTreeSet<Var> {
    let mut lent: BTreeSet<Var> = (f.params.iter().enumerate())
        .filter(|(i, _)| borrowed(*i))
        .map(|(_, p)| *p)
        .collect();
    // Variables are bound before they're used, and outer blocks before
    // inner ones, so one pass in that order finds them all.
    for block in f.body.blocks() {
        for l in &block.lets {
            let reads = matches!(l.op, Op::Get(..) | Op::ArrayGet | Op::Unbox(_));
            if let ([Atom::Var(x), ..], [y]) = (&l.args[..], &l.binds[..])
                && reads
                && lent.contains(x)
                && f.var_ty(*x).mutability() == Some(Mutability::Imm)
                && f.var_ty(*y).is_object()
            {
                lent.insert(*y);
            }
        }
    }
    lent
}

/// Whether something can unwind `f`'s frame while `l` runs: an abort,
/// which only code that may perform can make, or an exception from the
/// host, which only an extern with `throw` can.
///
/// So: a `perform`, `discontinue`, `resume` or `handle`, or a call or
/// `apply` of something whose row [unwinds](unwinds), in a function whose
/// own row does too, since what it performs is otherwise handled inside
/// it; or a `call_extern` whose row has `throw`.
pub fn may_unwind(m: &Module, f: &Func, l: &Let) -> bool {
    if !unwinds(m, &f.row) {
        return false;
    }
    match &l.op {
        Op::Perform(..) | Op::Discontinue(..) | Op::Resume | Op::Handle(..) => true,
        Op::Call(g, ..) => m.funcs.get(g).is_none_or(|g| unwinds(m, &g.row)),
        Op::CallExtern(e) => m.externs.get(e).is_none_or(|e| e.sig.row.throws()),
        Op::Apply => match l.args.first().map(|a| f.atom_ty(a)) {
            Some(Ty::Fn(sig, _)) => unwinds(m, &sig.row),
            _ => true,
        },
        _ => false,
    }
}

/// Whether code of row `row` can be unwound: it may perform an effect
/// that has operations, which an aborting handler can unwind, or has
/// `throw`, or is `Any`. An effect without operations, like an extern's
/// `io`, can't be performed, so it can't abort.
pub fn unwinds(m: &Module, row: &Row) -> bool {
    row.throws()
        || !row.params().is_empty()
        || (row.effects().iter()).any(|e| m.effects.get(e).is_none_or(|d| !d.ops.is_empty()))
}

/// The analysis of one counted function.
pub struct Owned<'a> {
    /// Whether each variable is counted.
    counted: Vec<bool>,
    /// What the body of each join point uses from outside, by label: what
    /// a jump to it hands over besides its arguments.
    free: Vec<BTreeSet<Var>>,
    /// Whether a function's parameter is borrowed.
    borrows: Box<dyn Fn(FuncId, usize) -> bool + 'a>,
}

impl<'a> Owned<'a> {
    /// The analysis of `f`, whose callees' borrowed parameters `borrows`
    /// gives: `borrows(g, i)` if `g`'s parameter `i` is.
    pub fn new(f: &Func, borrows: impl Fn(FuncId, usize) -> bool + 'a) -> Owned<'a> {
        let mut counted: Vec<bool> = f.vars.iter().map(|t| t.is_object()).collect();
        for v in lent(f, |i| f.is_borrowed(i)) {
            counted[v.0 as usize] = false;
        }
        for l in f.body.all_lets() {
            if matches!(l.op, Op::Const(_) | Op::Undefined) {
                for v in &l.binds {
                    counted[v.0 as usize] = false;
                }
            }
        }
        let mut owned = Owned {
            counted,
            free: vec![BTreeSet::new(); f.labels.max(1) as usize],
            borrows: Box::new(borrows),
        };
        owned.find_free(f);
        owned
    }

    /// What the frame owns while `block.lets[index]` runs, once it has
    /// taken what it takes: each counted variable that owns a reference,
    /// and how many. Not what the `let` binds, which it hasn't yet.
    pub fn during(&self, block: &Block, index: usize) -> Vec<(Var, u32)> {
        let Some(mut after) = self.lets(&block.lets[index + 1..], &block.tail) else {
            return Vec::new();
        };
        for v in &block.lets[index].binds {
            after.remove(v);
        }
        positive(after)
    }

    /// [`during`](Self::during) each of `block`'s `let`s, in one pass.
    pub fn during_each(&self, block: &Block) -> Vec<Vec<(Var, u32)>> {
        let mut out = vec![Vec::new(); block.lets.len()];
        let Some(mut after) = self.tail(&block.tail) else {
            return out;
        };
        for (i, l) in block.lets.iter().enumerate().rev() {
            let mut during = after.clone();
            for v in &l.binds {
                during.remove(v);
            }
            out[i] = positive(during);
            self.step_back(l, &mut after);
        }
        out
    }

    fn is(&self, v: Var) -> bool {
        self.counted.get(v.0 as usize).copied().unwrap_or(false)
    }

    /// What `lets` and then `tail` take, less what they `dup`, if they
    /// can finish.
    fn lets(&self, lets: &[Let], tail: &Tail) -> Option<Counts> {
        let mut c = self.tail(tail)?;
        for l in lets.iter().rev() {
            self.step_back(l, &mut c);
        }
        Some(c)
    }

    /// `c`, what's taken after `l`, becomes what's taken from before it.
    fn step_back(&self, l: &Let, c: &mut Counts) {
        for v in &l.binds {
            c.remove(v);
        }
        for (i, a) in l.args.iter().enumerate() {
            let Atom::Var(v) = a else { continue };
            if !self.is(*v) {
                continue;
            }
            let n = match &l.op {
                Op::Dup => -1,
                Op::Call(g, ..) if (self.borrows)(*g, i) => 0,
                op if op.takes(i) => 1,
                _ => 0,
            };
            if n != 0 {
                *c.entry(*v).or_default() += n;
            }
        }
    }

    fn tail(&self, tail: &Tail) -> Option<Counts> {
        crate::deep(|| match tail {
            Tail::Jump { target, args, .. } => {
                let mut c = Counts::new();
                for v in args.iter().filter_map(Atom::var).filter(|v| self.is(*v)) {
                    *c.entry(v).or_default() += 1;
                }
                if *target != Label::RET
                    && let Some(free) = self.free.get(target.0 as usize)
                {
                    for &v in free {
                        *c.entry(v).or_default() += 1;
                    }
                }
                Some(c)
            }
            Tail::Case { arms, default, .. } => {
                let branches = (arms.iter().map(|a| &a.body)).chain(default.as_deref());
                let mut least: Option<Counts> = None;
                for c in branches.filter_map(|b| self.lets(&b.lets, &b.tail)) {
                    least = Some(match least {
                        None => c,
                        Some(l) => min(l, c),
                    });
                }
                least
            }
            Tail::Join { body, .. } => self.lets(&body.lets, &body.tail),
            Tail::Unreachable | Tail::Trap => None,
        })
    }

    /// Finds [`free`](Self::free), as `insert_counts` does: what's live at
    /// the start of each join point's body, besides its parameters. A
    /// fixpoint, since a body can jump to itself, or to another join that
    /// uses more.
    fn find_free(&mut self, f: &Func) {
        let defs = join_defs(&f.body, f.labels);
        loop {
            let mut changed = false;
            for def in defs.iter().flatten() {
                let mut live = self.live(&def.body);
                for p in &def.params {
                    live.remove(p);
                }
                let slot = &mut self.free[def.label.0 as usize];
                if *slot != live {
                    *slot = live;
                    changed = true;
                }
            }
            if !changed {
                return;
            }
        }
    }

    /// The counted variables live when `b` starts.
    fn live(&self, b: &Block) -> BTreeSet<Var> {
        crate::deep(|| {
            let mut live = match &b.tail {
                Tail::Jump { target, args, .. } => {
                    let mut live: BTreeSet<Var> = args.iter().filter_map(Atom::var).collect();
                    if let Some(free) = self.free.get(target.0 as usize) {
                        live.extend(free);
                    }
                    live
                }
                Tail::Case {
                    scrut,
                    arms,
                    default,
                } => {
                    let mut live: BTreeSet<Var> = scrut.var().into_iter().collect();
                    for b in (arms.iter().map(|a| &a.body)).chain(default.as_deref()) {
                        live.extend(self.live(b));
                    }
                    live
                }
                Tail::Join { body, .. } => self.live(body),
                Tail::Unreachable | Tail::Trap => BTreeSet::new(),
            };
            for l in b.lets.iter().rev() {
                for v in &l.binds {
                    live.remove(v);
                }
                live.extend(l.args.iter().filter_map(Atom::var));
            }
            live.retain(|v| self.is(*v));
            live
        })
    }
}

/// The least of `a` and `b` for each variable, which is 0 where either
/// has none.
fn min(a: Counts, b: Counts) -> Counts {
    let vars: BTreeSet<Var> = a.keys().chain(b.keys()).copied().collect();
    vars.into_iter()
        .map(|v| {
            let n = a.get(&v).copied().unwrap_or(0);
            (v, n.min(b.get(&v).copied().unwrap_or(0)))
        })
        .collect()
}

fn positive(c: Counts) -> Vec<(Var, u32)> {
    c.into_iter()
        .filter(|&(_, n)| n > 0)
        .map(|(v, n)| (v, n as u32))
        .collect()
}
