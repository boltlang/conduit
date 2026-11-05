//! Removes copies: `ArrayClone` ops, and data objects rebuilt from their
//! own fields. See "Remove copies" in `docs/ir.md`.
//!
//! - **Clones that change nothing.** An `Imm` array cloned to `Imm` can't
//!   be told apart from the array itself, so the clone is the array, and
//!   its uses use the array instead.
//! - **Rebuilds that change nothing.** A `new C(get C.0(x), .., get C.n(x))`
//!   of an `Imm` object `x`, to `Imm` at the same type, gives an object
//!   with the same fields as `x`. Nothing compares objects by identity, so
//!   it's `x`. This is what a frontend's own copy to `Imm`, like Bolt's
//!   `to_imm`, comes down to on a value that's `Imm` already.
//! - **Freeze in place.** An `ArrayClone` to `Imm` becomes an
//!   [`Op::ArrayFreeze`] when nothing but the clone can reach the original array
//!   any more.
//!
//! Freezing needs a region analysis. Two variables are in the same region
//! if they may reach the same `Mut` object: an op that stores a value in an
//! object, reads one out, captures one or boxes one puts its arguments and
//! results in the same region. A region *escapes* if code outside this
//! function may hold one of its objects: parameters, values passed to or
//! returned from calls, returned values, and constants. Variables whose
//! type is `Imm` all the way down only reach objects that never change, so
//! they are left out: this is what keeps the clone's result out of the
//! original's region.
//!
//! An `ArrayClone` of `x` to `Imm` is frozen in place if `x`'s region
//! doesn't escape and no variable of the region is live after the clone.
//! Every object of the region was then made by this run of the function,
//! no object outside the region points at it, and nothing left in the
//! function can reach it, so the clone is the only way left to the array.
//! Liveness follows jumps into join points, so a loop that uses `x` again
//! on its next round keeps the clone.
//!
//! The analysis is flow-insensitive and doesn't look into callees, so any
//! call that sees a value makes its region escape.

use std::collections::{BTreeMap, BTreeSet};

use conduit_ir::verify::deep_imm;
use conduit_ir::{Atom, Block, Func, FuncId, Label, Let, Mutability, Op, Program, Tail, Ty, Var};

use crate::Pass;

/// The pass that removes copies.
pub struct Copies;

impl Pass for Copies {
    fn name(&self) -> &'static str {
        "copies"
    }

    fn run(&self, p: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        copies(p, f)
    }
}

/// Removes or freezes the copies of `f` that can be. Returns whether
/// anything changed.
pub fn copies(p: &dyn Program, f: &mut Func) -> bool {
    let removed = remove_noop_copies(f);
    let rebuilt = remove_rebuilds(f);
    let frozen = freeze_in_place(p, f);
    removed || rebuilt || frozen
}

/// Drops each clone of an `Imm` array to `Imm`, and uses its argument
/// instead of its result.
fn remove_noop_copies(f: &mut Func) -> bool {
    let mut subst = BTreeMap::new();
    let vars = &f.vars;
    for_each_block_mut(&mut f.body, &mut |b| {
        b.lets.retain(|l| {
            let (Op::ArrayClone, [Atom::Var(arg)], [bind]) = (&l.op, &l.args[..], &l.binds[..])
            else {
                return true;
            };
            let arg_ty = &vars[arg.0 as usize];
            if matches!(arg_ty, Ty::Array(_, Mutability::Imm)) && vars[bind.0 as usize] == *arg_ty {
                let arg = Atom::Var(*arg);
                subst.insert(*bind, arg);
                return false;
            }
            true
        })
    });
    if subst.is_empty() {
        return false;
    }
    substitute(&mut f.body, &subst);
    true
}

/// Drops each rebuild of an `Imm` object from its own fields, and uses the
/// object instead of the rebuild.
fn remove_rebuilds(f: &mut Func) -> bool {
    // What each field read reads: the object, the constructor, the field.
    let mut gets = BTreeMap::new();
    let mut news = Vec::new();
    for_each_block(&f.body, &mut |b| {
        for l in &b.lets {
            match (&l.op, &l.args[..], &l.binds[..]) {
                (Op::Get(c, i), [Atom::Var(x)], [v]) => {
                    gets.insert(*v, (*x, *c, *i));
                }
                (Op::New(c), args, [y])
                    if !args.is_empty()
                        && matches!(f.vars[y.0 as usize], Ty::Data(_, Mutability::Imm, ..)) =>
                {
                    news.push((*y, *c, args.to_vec()));
                }
                _ => {}
            }
        }
    });

    // A rebuild's fields can be rebuilds themselves, so this goes on until
    // nothing more is found.
    let mut subst: BTreeMap<Var, Atom> = BTreeMap::new();
    let resolve = |subst: &BTreeMap<Var, Atom>, mut v: Var| {
        while let Some(Atom::Var(to)) = subst.get(&v) {
            v = *to;
        }
        v
    };
    loop {
        let mut found = false;
        for (y, c, args) in &news {
            if subst.contains_key(y) {
                continue;
            }
            let mut object = None;
            let rebuilds = args.iter().enumerate().all(|(i, a)| {
                let Atom::Var(v) = a else { return false };
                let Some(&(x, gc, gi)) = gets.get(&resolve(&subst, *v)) else {
                    return false;
                };
                let x = resolve(&subst, x);
                gc == *c && gi == i as u32 && *object.get_or_insert(x) == x
            });
            if let Some(x) = object.filter(|_| rebuilds)
                && f.vars[x.0 as usize] == f.vars[y.0 as usize]
            {
                subst.insert(*y, Atom::Var(x));
                found = true;
            }
        }
        if !found {
            break;
        }
    }
    if subst.is_empty() {
        return false;
    }
    for_each_block_mut(&mut f.body, &mut |b| {
        b.lets
            .retain(|l| !l.binds.iter().any(|v| subst.contains_key(v)));
    });
    substitute(&mut f.body, &subst);
    true
}

/// Turns each `ArrayClone` to `Imm` that is the only way left to its array
/// into a [`Op::ArrayFreeze`].
fn freeze_in_place(p: &dyn Program, f: &mut Func) -> bool {
    let tracked: Vec<bool> = f.vars.iter().map(|ty| !deep_imm(p, ty)).collect();
    let regions = Regions::new(f, &tracked);

    // The clones that might be frozen: by their result, the region of
    // their argument.
    let mut candidates = BTreeMap::new();
    for_each_block(&f.body, &mut |b| {
        for l in &b.lets {
            if let (Op::ArrayClone, [Atom::Var(x)], [y]) = (&l.op, &l.args[..], &l.binds[..])
                && matches!(f.vars[y.0 as usize], Ty::Array(_, Mutability::Imm))
                && tracked[x.0 as usize]
                && regions.of(*x) != regions.escaped
            {
                candidates.insert(*y, regions.of(*x));
            }
        }
    });
    if candidates.is_empty() {
        return false;
    }

    let mut live = Liveness {
        tracked: &tracked,
        regions: &regions,
        label_live: vec![BTreeSet::new(); f.labels.max(1) as usize],
        candidates: &candidates,
        blocked: BTreeSet::new(),
    };
    live.block(&f.body);
    let blocked = live.blocked;

    let mut changed = false;
    for_each_block_mut(&mut f.body, &mut |b| {
        for l in &mut b.lets {
            if let [y] = l.binds[..]
                && l.op == Op::ArrayClone
                && candidates.contains_key(&y)
                && !blocked.contains(&y)
            {
                l.op = Op::ArrayFreeze;
                changed = true;
            }
        }
    });
    changed
}

/// The region of every variable, named by a representative.
struct Regions {
    region: Vec<usize>,
    /// The region of everything code outside the function may hold.
    escaped: usize,
}

impl Regions {
    /// Puts the tracked variables of `f` in regions. Untracked ones are
    /// left in regions of their own.
    fn new(f: &Func, tracked: &[bool]) -> Regions {
        let escaped = f.vars.len();
        let mut cx = RegionCx {
            tracked,
            parent: (0..=escaped).collect(),
            label_params: vec![Vec::new(); f.labels.max(1) as usize],
            escaped,
        };
        for_each_block(&f.body, &mut |b| {
            if let Tail::Join { defs, .. } = &b.tail {
                for d in defs {
                    cx.label_params[d.label.0 as usize] = d.params.clone();
                }
            }
        });
        for &p in &f.params {
            cx.escape(p);
        }
        for_each_block(&f.body, &mut |b| cx.block(b));
        Regions {
            region: (0..=escaped).map(|v| cx.find(v)).collect(),
            escaped: cx.find(escaped),
        }
    }

    fn of(&self, v: Var) -> usize {
        self.region[v.0 as usize]
    }
}

struct RegionCx<'a> {
    tracked: &'a [bool],
    /// A union-find forest over the variables, plus one node for the
    /// escaped region.
    parent: Vec<usize>,
    label_params: Vec<Vec<Var>>,
    escaped: usize,
}

impl RegionCx<'_> {
    fn find(&mut self, mut v: usize) -> usize {
        while self.parent[v] != v {
            self.parent[v] = self.parent[self.parent[v]];
            v = self.parent[v];
        }
        v
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[a] = b;
        }
    }

    fn is_tracked(&self, v: Var) -> bool {
        self.tracked[v.0 as usize]
    }

    fn escape(&mut self, v: Var) {
        if self.is_tracked(v) {
            self.union(v.0 as usize, self.escaped);
        }
    }

    /// Puts all tracked variables among `vars` in one region.
    fn unite(&mut self, vars: impl IntoIterator<Item = Var>) {
        let mut first: Option<Var> = None;
        for v in vars {
            if !self.is_tracked(v) {
                continue;
            }
            match first {
                None => first = Some(v),
                Some(f) => self.union(f.0 as usize, v.0 as usize),
            }
        }
    }

    /// The `let`s and tail of `b`, but not the blocks nested in it.
    fn block(&mut self, b: &Block) {
        for l in &b.lets {
            self.let_(l);
        }
        if let Tail::Jump { target, args, .. } = &b.tail {
            if *target == Label::RET {
                for v in vars(args) {
                    self.escape(v);
                }
            } else {
                let params = std::mem::take(&mut self.label_params[target.0 as usize]);
                for (a, &param) in args.iter().zip(&params) {
                    if let Atom::Var(v) = a {
                        self.unite([*v, param]);
                    }
                }
                self.label_params[target.0 as usize] = params;
            }
        }
    }

    fn let_(&mut self, l: &Let) {
        let all = vars(&l.args).chain(l.binds.iter().copied());
        match &l.op {
            // Code we don't see gets the arguments and gives the results.
            // A constant's object is a global's too.
            Op::Const(_)
            | Op::Call(..)
            | Op::CallExtern(_)
            | Op::Apply
            | Op::Handle(..)
            | Op::Perform(..)
            | Op::Resume
            | Op::Discontinue(..) => {
                for v in all {
                    self.escape(v);
                }
            }
            // Anything else may link its arguments and results: `New`,
            // `Set`, `Get` and the like store and load them, `Closure` and
            // `Box` hold them, and an `ArrayClone` shares the elements. An
            // `Imm` result isn't tracked, and neither are scalars, so those
            // link nothing.
            _ => self.unite(all),
        }
    }
}

/// Finds the candidate copies that a variable of their argument's region
/// is live after.
struct Liveness<'a> {
    tracked: &'a [bool],
    regions: &'a Regions,
    /// The tracked variables live where each join point starts, other than
    /// its parameters.
    label_live: Vec<BTreeSet<Var>>,
    candidates: &'a BTreeMap<Var, usize>,
    blocked: BTreeSet<Var>,
}

impl Liveness<'_> {
    fn add_uses(&self, live: &mut BTreeSet<Var>, atoms: &[Atom]) {
        live.extend(vars(atoms).filter(|v| self.tracked[v.0 as usize]));
    }

    /// The tracked variables live where `b` starts.
    fn block(&mut self, b: &Block) -> BTreeSet<Var> {
        let mut live = self.tail(&b.tail);
        for l in b.lets.iter().rev() {
            // `live` is what's live after `l`.
            if let [y] = l.binds[..]
                && let Some(&r) = self.candidates.get(&y)
                && live.iter().any(|&v| self.regions.of(v) == r)
            {
                self.blocked.insert(y);
            }
            for b in &l.binds {
                live.remove(b);
            }
            self.add_uses(&mut live, &l.args);
        }
        live
    }

    fn tail(&mut self, t: &Tail) -> BTreeSet<Var> {
        match t {
            Tail::Jump { target, args, .. } => {
                let mut live = self.label_live[target.0 as usize].clone();
                self.add_uses(&mut live, args);
                live
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                let mut live = BTreeSet::new();
                for a in arms {
                    live.extend(self.block(&a.body));
                }
                if let Some(d) = default {
                    live.extend(self.block(d));
                }
                self.add_uses(&mut live, std::slice::from_ref(scrut));
                live
            }
            Tail::Unreachable | Tail::Trap => BTreeSet::new(),
            Tail::Join { rec, defs, body } => {
                // Live sets only grow, so a recursive group reaches a fixed
                // point. A copy blocked on an earlier round would be on the
                // last one too, so blocking as we go is fine.
                loop {
                    let mut changed = false;
                    for d in defs {
                        let mut live = self.block(&d.body);
                        for p in &d.params {
                            live.remove(p);
                        }
                        let slot = &mut self.label_live[d.label.0 as usize];
                        if live != *slot {
                            *slot = live;
                            changed = true;
                        }
                    }
                    if !rec || !changed {
                        break;
                    }
                }
                self.block(body)
            }
        }
    }
}

fn vars(atoms: &[Atom]) -> impl Iterator<Item = Var> + '_ {
    atoms.iter().filter_map(|a| match a {
        Atom::Var(v) => Some(*v),
        Atom::Lit(_) => None,
    })
}

/// Calls `f` on `b` and every block nested in it.
fn for_each_block(b: &Block, f: &mut impl FnMut(&Block)) {
    f(b);
    match &b.tail {
        Tail::Join { defs, body, .. } => {
            for d in defs {
                for_each_block(&d.body, f);
            }
            for_each_block(body, f);
        }
        Tail::Case { arms, default, .. } => {
            for a in arms {
                for_each_block(&a.body, f);
            }
            if let Some(d) = default {
                for_each_block(d, f);
            }
        }
        Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
    }
}

/// Calls `f` on `b` and every block nested in it.
fn for_each_block_mut(b: &mut Block, f: &mut impl FnMut(&mut Block)) {
    f(b);
    match &mut b.tail {
        Tail::Join { defs, body, .. } => {
            for d in defs {
                for_each_block_mut(&mut d.body, f);
            }
            for_each_block_mut(body, f);
        }
        Tail::Case { arms, default, .. } => {
            for a in arms {
                for_each_block_mut(&mut a.body, f);
            }
            if let Some(d) = default {
                for_each_block_mut(d, f);
            }
        }
        Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
    }
}

/// Replaces every use of a variable in `subst` with what it maps to.
fn substitute(b: &mut Block, subst: &BTreeMap<Var, Atom>) {
    // A copy's argument may itself be a removed copy's result.
    let resolve = |a: &mut Atom| {
        while let Atom::Var(v) = a
            && let Some(to) = subst.get(v)
        {
            *a = to.clone();
        }
    };
    for_each_block_mut(b, &mut |b| {
        for l in &mut b.lets {
            l.args.iter_mut().for_each(resolve);
        }
        match &mut b.tail {
            Tail::Jump { args, .. } => args.iter_mut().for_each(resolve),
            Tail::Case { scrut, .. } => resolve(scrut),
            Tail::Join { .. } | Tail::Unreachable | Tail::Trap => {}
        }
    });
}
