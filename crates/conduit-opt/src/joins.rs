//! Inlines join points, which is how case-of-case comes about in this IR.
//! See "Case-of-case" in `docs/ir.md`.
//!
//! - **Used once.** A join definition that only one `jump` goes to is
//!   moved there, with its parameters replaced by the jump's arguments. It
//!   runs as often as before, so code doesn't grow.
//! - **Known arguments.** A small join definition is copied into a jump
//!   that gives it a known value, a literal or an object made by `new` or
//!   `const`, for a parameter that it `case`s on. `simplify` then picks the
//!   arm. This is what makes inlining a function that returns a value
//!   built by `new` pay off: the rest of the caller is a join point, and
//!   its `case` on the result would otherwise never see the `new`.
//!
//! Recursive definitions stay, since copying one into a jump would unroll
//! the loop a level each run. So do definitions that mention a
//! continuation, which the verifier counts the uses of along each path.
//!
//! A copy gets fresh variables and labels for what it binds, keeping their
//! names, and shares the rest with the original.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use conduit_ir::const_value;
use conduit_ir::{
    Arm, Atom, Block, ConstValue, Func, FuncId, JoinDef, Label, Op, Program, Tail, Ty, Var,
};

use crate::Pass;
use crate::{MAX_CALLEE_SIZE, MAX_CALLER_SIZE};

/// The join point inlining pass.
pub struct Joins;

impl Pass for Joins {
    fn name(&self) -> &'static str {
        "joins"
    }

    fn run(&self, p: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        inline_joins(p, f)
    }
}

/// Inlines the join points of `f`, a function of `p`, that are worth it.
/// Returns whether anything changed.
pub fn inline_joins(p: &dyn Program, f: &mut Func) -> bool {
    let mut jumps = HashMap::new();
    let mut known = BTreeSet::new();
    let mut size = 0;
    f.body.walk(|b| {
        size += b.lets.len() + 1;
        for l in &b.lets {
            let is_known = match (&l.op, &l.binds[..]) {
                (Op::New(_), [_]) => true,
                (Op::Const(id), [_]) => {
                    matches!(
                        const_value(p, *id),
                        Some(ConstValue::New(..) | ConstValue::Lit(_))
                    )
                }
                _ => false,
            };
            if is_known {
                known.insert(l.binds[0]);
            }
        }
        if let Tail::Jump { target, .. } = &b.tail {
            *jumps.entry(*target).or_insert(0usize) += 1;
        }
    });

    // The definitions this run moves: those of a `join` that isn't
    // recursive, which only one jump goes to.
    let mut moved = BTreeSet::new();
    f.body.walk(|b| {
        if let Tail::Join {
            rec: false, defs, ..
        } = &b.tail
        {
            let once = defs.iter().filter(|d| jumps.get(&d.label) == Some(&1));
            moved.extend(once.map(|d| d.label));
        }
    });

    // The definitions that may be copied, as they are before this run
    // changes them, and the parameters they `case` on.
    let mut copyable = HashMap::new();
    f.body.walk(|b| {
        let Tail::Join {
            rec: false, defs, ..
        } = &b.tail
        else {
            return;
        };
        for d in defs {
            if jumps.get(&d.label).copied().unwrap_or(0) < 2 || d.body.size() > MAX_CALLEE_SIZE {
                continue;
            }
            let scrutinized: Vec<usize> = (0..d.params.len())
                .filter(|&i| cases_on(&d.body, d.params[i]))
                .collect();
            if scrutinized.is_empty() || mentions_cont(&d.body, &f.vars) {
                continue;
            }
            // A copy adds jumps to the labels the definition jumps to, so
            // if this run moves one of those to its only jump, the copy
            // would be left without it.
            let inner = defined_labels(&d.body);
            let mut jumps_to_moved = false;
            d.body.walk(|b| {
                if let Tail::Jump { target, .. } = &b.tail {
                    jumps_to_moved |= !inner.contains(target) && moved.contains(target);
                }
            });
            if !jumps_to_moved {
                copyable.insert(d.label, (d.clone(), scrutinized));
            }
        }
    });

    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    let mut cx = Cx {
        jumps,
        known,
        copyable,
        moving: HashMap::new(),
        size,
        changed: false,
    };
    f.body = cx.block(body, f);
    cx.changed
}

struct Cx {
    /// How many jumps go to each label, before this run.
    jumps: HashMap<Label, usize>,
    /// The variables bound by `new`, or by `const` of an object or a
    /// literal.
    known: BTreeSet<Var>,
    /// The definitions that may be copied, and the parameters they `case`
    /// on.
    copyable: HashMap<Label, (JoinDef, Vec<usize>)>,
    /// The definitions used once, taken out of their `join` until their
    /// jump is reached.
    moving: HashMap<Label, JoinDef>,
    /// The function's size, in `let`s and tails.
    size: usize,
    changed: bool,
}

impl Cx {
    fn block(&mut self, mut b: Block, f: &mut Func) -> Block {
        loop {
            match self.tail(b.tail, f) {
                Ok(tail) => {
                    b.tail = tail;
                    return b;
                }
                // A definition in place of a jump: its `let`s go after
                // these, and its tail is looked at in turn.
                Err(def) => {
                    self.changed = true;
                    b.lets.extend(def.lets);
                    b.tail = def.tail;
                }
            }
        }
    }

    /// `t` with its join points inlined, or `Err` with the block that
    /// replaces it if it's a jump to one.
    fn tail(&mut self, t: Tail, f: &mut Func) -> Result<Tail, Block> {
        Ok(match t {
            Tail::Join { rec, defs, body } => {
                let mut kept = Vec::new();
                for d in defs {
                    if !rec && self.jumps.get(&d.label) == Some(&1) {
                        self.moving.insert(d.label, d);
                    } else {
                        kept.push(d);
                    }
                }
                let kept: Vec<JoinDef> = kept
                    .into_iter()
                    .map(|d| JoinDef {
                        body: self.block(d.body, f),
                        ..d
                    })
                    .collect();
                let body = self.block(*body, f);
                if kept.is_empty() {
                    return Err(body);
                }
                Tail::Join {
                    rec,
                    defs: kept,
                    body: Box::new(body),
                }
            }
            Tail::Jump {
                target,
                args,
                source,
            } => {
                if let Some(d) = self.moving.remove(&target) {
                    let mut s = Subst::default();
                    s.vars.extend(d.params.iter().copied().zip(args));
                    return Err(s.block(d.body));
                }
                if let Some(copy) = self.copy(target, &args, f) {
                    return Err(copy);
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
                scrut,
                arms: arms
                    .into_iter()
                    .map(|a| Arm {
                        tag: a.tag,
                        body: self.block(a.body, f),
                    })
                    .collect(),
                default: default.map(|d| Box::new(self.block(*d, f))),
            },
            Tail::Unreachable => Tail::Unreachable,
            Tail::Trap => Tail::Trap,
        })
    }

    /// A copy of the definition of `target` for a jump with `args`, if one
    /// of the parameters it `case`s on gets a known value.
    fn copy(&mut self, target: Label, args: &[Atom], f: &mut Func) -> Option<Block> {
        let (d, scrutinized) = self.copyable.get(&target)?;
        let is_known = |a: &Atom| match a {
            Atom::Lit(_) => true,
            Atom::Var(v) => self.known.contains(v),
        };
        if !scrutinized
            .iter()
            .any(|&i| args.get(i).is_some_and(is_known))
        {
            return None;
        }
        let grown = d.body.size();
        if self.size + grown > MAX_CALLER_SIZE {
            return None;
        }
        self.size += grown;
        let mut s = Subst::default();
        s.vars
            .extend(d.params.iter().copied().zip(args.iter().cloned()));
        // Fresh variables and labels for what the copy binds and defines.
        d.body.walk(|b| {
            for l in &b.lets {
                s.fresh.extend(l.binds.iter().copied());
            }
            if let Tail::Join { defs, .. } = &b.tail {
                for d in defs {
                    s.fresh.extend(d.params.iter().copied());
                    s.fresh_labels.push(d.label);
                }
            }
        });
        for v in std::mem::take(&mut s.fresh) {
            let new = f.new_var(f.var_ty(v).clone());
            if let Some(name) = f.debug.vars.get(&v).cloned() {
                f.debug.vars.insert(new, name);
            }
            s.vars.insert(v, Atom::Var(new));
        }
        for l in std::mem::take(&mut s.fresh_labels) {
            let new = f.new_label();
            if let Some(name) = f.debug.labels.get(&l).cloned() {
                f.debug.labels.insert(new, name);
            }
            s.labels.insert(l, new);
        }
        Some(s.block(d.body.clone()))
    }
}

/// Replaces variables and labels in a block, leaving the others as they
/// are.
#[derive(Default)]
struct Subst {
    vars: BTreeMap<Var, Atom>,
    labels: BTreeMap<Label, Label>,
    fresh: BTreeSet<Var>,
    fresh_labels: Vec<Label>,
}

impl Subst {
    fn atom(&self, a: &mut Atom) {
        if let Atom::Var(v) = a
            && let Some(to) = self.vars.get(v)
        {
            *a = to.clone();
        }
    }

    fn var(&self, v: &mut Var) {
        if let Some(Atom::Var(to)) = self.vars.get(v) {
            *v = *to;
        }
    }

    fn label(&self, l: &mut Label) {
        if let Some(to) = self.labels.get(l) {
            *l = *to;
        }
    }

    fn block(&self, mut b: Block) -> Block {
        for l in &mut b.lets {
            l.args.iter_mut().for_each(|a| self.atom(a));
            l.binds.iter_mut().for_each(|v| self.var(v));
        }
        b.tail = match b.tail {
            Tail::Join { rec, defs, body } => Tail::Join {
                rec,
                defs: defs
                    .into_iter()
                    .map(|mut d| {
                        self.label(&mut d.label);
                        d.params.iter_mut().for_each(|v| self.var(v));
                        JoinDef {
                            body: self.block(d.body),
                            ..d
                        }
                    })
                    .collect(),
                body: Box::new(self.block(*body)),
            },
            Tail::Jump {
                mut target,
                mut args,
                source,
            } => {
                self.label(&mut target);
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
            Tail::Unreachable => Tail::Unreachable,
            Tail::Trap => Tail::Trap,
        };
        b
    }
}

/// Whether a `case` in `b` is on `v`.
fn cases_on(b: &Block, v: Var) -> bool {
    let mut found = false;
    b.walk(|b| {
        found |= matches!(&b.tail, Tail::Case { scrut: Atom::Var(s), .. } if *s == v);
    });
    found
}

/// The labels `b` defines.
fn defined_labels(b: &Block) -> BTreeSet<Label> {
    let mut labels = BTreeSet::new();
    b.walk(|b| {
        if let Tail::Join { defs, .. } = &b.tail {
            labels.extend(defs.iter().map(|d| d.label));
        }
    });
    labels
}

/// Whether `b` uses or binds a variable of a continuation type.
fn mentions_cont(b: &Block, vars: &[Ty]) -> bool {
    let is_cont = |v: &Var| matches!(vars.get(v.0 as usize), Some(Ty::Cont(_)));
    let atom_cont = |a: &Atom| matches!(a, Atom::Var(v) if is_cont(v));
    let mut found = false;
    b.walk(|b| {
        for l in &b.lets {
            found |= l.args.iter().any(atom_cont) || l.binds.iter().any(is_cont);
        }
        found |= match &b.tail {
            Tail::Jump { args, .. } => args.iter().any(atom_cont),
            Tail::Case { scrut, .. } => atom_cont(scrut),
            Tail::Join { defs, .. } => defs.iter().any(|d| d.params.iter().any(is_cont)),
            Tail::Unreachable | Tail::Trap => false,
        };
    });
    found
}
