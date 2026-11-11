//! Ref promotion, like LLVM's `mem2reg`: a ref that doesn't escape becomes
//! the values it holds, passed between join points as parameters.
//!
//! Frontends turn each mutable local into a ref (rule 6 in `docs/ir.md`),
//! so this is what turns them back into plain values. A ref is promoted
//! when a `ref_new` in the function makes it and its only uses are as the
//! first argument of `ref_get` and `ref_set`: it isn't captured, passed to
//! a call or a jump, stored, boxed or returned. Then:
//!
//! - `ref_new(v)` and `ref_set(r, v)` make `v` the ref's current value,
//!   and `ref_get(r)` is the current value, put in place of what it binds.
//! - A join point gets a parameter for each ref whose value it needs: one
//!   that a path from its start reads before writing. Each jump to it
//!   passes the current values.
//!
//! The values a join point needs are found by a backward liveness
//! analysis, repeated over all join points until nothing changes, so that
//! loops and join points that jump to each other get their parameters.
//!
//! A ref of a continuation isn't promoted: the verifier counts the uses of
//! a continuation along each path, and a program that reads one out of a
//! ref on paths that never both run could then look like it uses it
//! twice.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use conduit_ir::{Atom, Block, Func, FuncId, JoinDef, Op, Program, Tail, Ty, Var};

use crate::Pass;

/// The ref promotion pass.
pub struct Promote;

impl Pass for Promote {
    fn name(&self) -> &'static str {
        "promote"
    }

    fn run(&self, _: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        promote(f)
    }
}

/// Promotes every ref of `f` that doesn't escape. Returns whether anything
/// changed.
///
/// Variables that are no longer bound stay in `f`'s tables, and the new
/// join parameters are added to them.
pub fn promote(f: &mut Func) -> bool {
    let refs = promotable(f);
    if refs.is_empty() {
        return false;
    }
    let needs = needs(f, &refs);
    let mut params = vec![Vec::new(); f.labels.max(1) as usize];
    for (l, rs) in needs.iter().enumerate() {
        for &r in rs {
            let Ty::Ref(inner) = f.var_ty(r) else {
                unreachable!("only refs are promoted");
            };
            let inner = (**inner).clone();
            let value = f.new_var(inner);
            // The value is the ref's, so it gets the ref's name.
            if let Some(name) = f.debug.vars.get(&r).cloned() {
                f.debug.vars.insert(value, name);
            }
            params[l].push((r, value));
        }
    }
    let mut cx = Rewrite {
        refs: &refs,
        params,
        subst: HashMap::new(),
    };
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    f.body = cx.block(body, &mut BTreeMap::new());
    true
}

/// The refs that a `ref_new` makes and that are only read and written.
fn promotable(f: &Func) -> BTreeSet<Var> {
    let mut made = BTreeSet::new();
    let mut escaped = BTreeSet::new();
    let escape = |atoms: &[Atom], escaped: &mut BTreeSet<Var>| {
        escaped.extend(atoms.iter().filter_map(Atom::var));
    };
    for b in f.body.blocks() {
        for l in &b.lets {
            match l.op {
                Op::RefNew => {
                    made.extend(l.binds.iter().copied());
                    escape(&l.args, &mut escaped);
                }
                // The ref itself is read or written; a value it gets is
                // stored in it.
                Op::RefGet | Op::RefSet => {
                    escape(l.args.get(1..).unwrap_or_default(), &mut escaped)
                }
                _ => escape(&l.args, &mut escaped),
            }
        }
        escape(b.tail.atoms(), &mut escaped);
    }
    made.retain(|v| {
        !escaped.contains(v) && matches!(f.vars.get(v.0 as usize), Some(Ty::Ref(inner)) if !matches!(**inner, Ty::Cont(_)))
    });
    made
}

/// For each label, the promoted refs whose value a path from the start of
/// its join definition reads before writing.
fn needs(f: &Func, refs: &BTreeSet<Var>) -> Vec<BTreeSet<Var>> {
    let mut defs: Vec<&JoinDef> = Vec::new();
    for b in f.body.blocks() {
        if let Tail::Join { defs: ds, .. } = &b.tail {
            defs.extend(ds);
        }
    }
    let mut needs = vec![BTreeSet::new(); f.labels.max(1) as usize];
    // The sets only grow, and are bounded by `refs`, so this ends.
    loop {
        let mut changed = false;
        for def in &defs {
            let live = live_in(&def.body, refs, &needs);
            let slot = &mut needs[def.label.0 as usize];
            if *slot != live {
                *slot = live;
                changed = true;
            }
        }
        if !changed {
            return needs;
        }
    }
}

/// The promoted refs that a path from the start of `b` reads before
/// writing, given what each label needs.
fn live_in(b: &Block, refs: &BTreeSet<Var>, needs: &[BTreeSet<Var>]) -> BTreeSet<Var> {
    conduit_ir::deep(|| {
        let mut live = match &b.tail {
            Tail::Jump { target, .. } => needs[target.0 as usize].clone(),
            Tail::Case { arms, default, .. } => {
                let mut live = BTreeSet::new();
                for arm in arms {
                    live.extend(live_in(&arm.body, refs, needs));
                }
                if let Some(d) = default {
                    live.extend(live_in(d, refs, needs));
                }
                live
            }
            // The definitions only run when jumped to.
            Tail::Join { body, .. } => live_in(body, refs, needs),
            Tail::Unreachable | Tail::Trap => BTreeSet::new(),
        };
        for l in b.lets.iter().rev() {
            match (&l.op, l.args.first()) {
                (Op::RefNew, _) => {
                    for v in &l.binds {
                        live.remove(v);
                    }
                }
                (Op::RefSet, Some(Atom::Var(r))) => {
                    live.remove(r);
                }
                (Op::RefGet, Some(Atom::Var(r))) if refs.contains(r) => {
                    live.insert(*r);
                }
                _ => {}
            }
        }
        live
    })
}

struct Rewrite<'a> {
    refs: &'a BTreeSet<Var>,
    /// For each label, the refs its join point now takes, each with its
    /// new parameter.
    params: Vec<Vec<(Var, Var)>>,
    /// What each variable a removed `ref_get` bound is now.
    subst: HashMap<Var, Atom>,
}

/// The current value of each promoted ref.
type Current = BTreeMap<Var, Atom>;

impl Rewrite<'_> {
    fn atom(&self, a: Atom) -> Atom {
        match a {
            Atom::Var(v) => self.subst.get(&v).cloned().unwrap_or(a),
            a => a,
        }
    }

    fn atoms(&self, atoms: Vec<Atom>) -> Vec<Atom> {
        atoms.into_iter().map(|a| self.atom(a)).collect()
    }

    fn current(cur: &Current, r: Var) -> Atom {
        // The analysis gives a join point a parameter for every ref it
        // reads before writing, so a ref always has a value where it's
        // read.
        cur.get(&r)
            .cloned()
            .unwrap_or_else(|| panic!("{r} is read where it has no value"))
    }

    fn block(&mut self, b: Block, cur: &mut Current) -> Block {
        conduit_ir::deep(|| {
            let mut lets = Vec::with_capacity(b.lets.len());
            for mut l in b.lets {
                l.args = self.atoms(l.args);
                let promoted = |a: Option<&Atom>| match a {
                    Some(Atom::Var(r)) if self.refs.contains(r) => Some(*r),
                    _ => None,
                };
                match l.op {
                    Op::RefNew if l.binds.len() == 1 && self.refs.contains(&l.binds[0]) => {
                        cur.insert(l.binds[0], l.args[0].clone());
                    }
                    Op::RefGet if let Some(r) = promoted(l.args.first()) => {
                        let value = Self::current(cur, r);
                        for b in l.binds {
                            self.subst.insert(b, value.clone());
                        }
                    }
                    Op::RefSet if let Some(r) = promoted(l.args.first()) => {
                        cur.insert(r, l.args[1].clone());
                    }
                    _ => lets.push(l),
                }
            }
            let tail = match b.tail {
                Tail::Jump {
                    target,
                    args,
                    source,
                } => {
                    let mut args = self.atoms(args);
                    for &(r, _) in &self.params[target.0 as usize] {
                        args.push(Self::current(cur, r));
                    }
                    Tail::Jump {
                        target,
                        args,
                        source,
                    }
                }
                Tail::Case {
                    scrut,
                    arms,
                    default,
                } => Tail::Case {
                    scrut: self.atom(scrut),
                    arms: arms
                        .into_iter()
                        .map(|mut arm| {
                            arm.body = self.block(arm.body, &mut cur.clone());
                            arm
                        })
                        .collect(),
                    default: default.map(|d| Box::new(self.block(*d, &mut cur.clone()))),
                },
                Tail::Join { rec, defs, body } => {
                    let defs = defs
                        .into_iter()
                        .map(|mut def| {
                            // A definition starts with what's passed to it.
                            let mut start = Current::new();
                            for &(r, p) in &self.params[def.label.0 as usize] {
                                def.params.push(p);
                                start.insert(r, Atom::Var(p));
                            }
                            def.body = self.block(def.body, &mut start);
                            def
                        })
                        .collect();
                    Tail::Join {
                        rec,
                        defs,
                        body: Box::new(self.block(*body, cur)),
                    }
                }
                Tail::Unreachable => Tail::Unreachable,
                Tail::Trap => Tail::Trap,
            };
            Block { lets, tail }
        })
    }
}
