//! Copying code from one function into another, as inlining and running
//! handlers directly do: a callee's body, or a clause's, with its
//! parameters replaced by what's passed, fresh variables and labels for the
//! rest, and its mutability and effect parameters by the arguments.

use std::collections::BTreeMap;

use conduit_ir::types::subst_all;
use conduit_ir::{
    Arm, Atom, Block, Func, JoinDef, Label, Let, Mutability, Op, Row, SourceId, Tail, Var,
};

/// Whether some op in `b` is one that `is` holds for.
pub(crate) fn has_op(b: &Block, is: &dyn Fn(&Op) -> bool) -> bool {
    b.blocks().any(|b| b.lets.iter().any(|l| is(&l.op)))
}

/// A copy of a callee's body for the caller.
pub(crate) struct Rename<'a> {
    /// What each callee variable is in the caller: an argument for a
    /// parameter, else a fresh variable.
    pub vars: BTreeMap<Var, Atom>,
    pub labels: BTreeMap<Label, Label>,
    pub margs: Vec<Mutability>,
    /// The rows the call gives for the callee's effect parameters.
    pub rows: Vec<Row>,
    pub source: Option<SourceId>,
    pub callee: &'a Func,
}

impl Rename<'_> {
    fn atom(&mut self, a: Atom, f: &mut Func) -> Atom {
        match a {
            Atom::Var(v) => self.var_atom(v, f),
            lit => lit,
        }
    }

    fn atoms(&mut self, atoms: &[Atom], f: &mut Func) -> Vec<Atom> {
        atoms.iter().map(|a| self.atom(a.clone(), f)).collect()
    }

    fn var_atom(&mut self, v: Var, f: &mut Func) -> Atom {
        if let Some(a) = self.vars.get(&v) {
            return a.clone();
        }
        let ty = subst_all(self.callee.var_ty(v), &self.margs, &self.rows);
        let new = f.new_var(ty);
        if let Some(name) = self.callee.debug.vars.get(&v) {
            f.debug.vars.insert(new, name.clone());
        }
        let a = Atom::Var(new);
        self.vars.insert(v, a.clone());
        a
    }

    /// A variable the callee binds, which is never a parameter.
    fn var(&mut self, v: Var, f: &mut Func) -> Var {
        match self.var_atom(v, f) {
            Atom::Var(v) => v,
            Atom::Lit(_) => unreachable!("a bound variable is never a parameter"),
        }
    }

    fn label(&mut self, l: Label, f: &mut Func) -> Label {
        if let Some(&new) = self.labels.get(&l) {
            return new;
        }
        let new = f.new_label();
        if let Some(name) = self.callee.debug.labels.get(&l) {
            f.debug.labels.insert(new, name.clone());
        }
        self.labels.insert(l, new);
        new
    }

    fn mutability(&self, m: Mutability) -> Mutability {
        match m {
            Mutability::Param(i) => self.margs.get(i as usize - 1).copied().unwrap_or(m),
            m => m,
        }
    }

    fn op(&self, op: &Op) -> Op {
        let margs = |ms: &[Mutability]| ms.iter().map(|&m| self.mutability(m)).collect();
        let rows = |rs: &[Row]| rs.iter().map(|r| r.subst(&self.rows)).collect();
        match op {
            Op::Call(g, ms, rs) => Op::Call(*g, margs(ms), rows(rs)),
            Op::Closure(g, ms, rs) => Op::Closure(*g, margs(ms), rows(rs)),
            Op::Handle(h, rs) => Op::Handle(*h, rows(rs)),
            Op::Box(ty) => Op::Box(subst_all(ty, &self.margs, &self.rows)),
            Op::Unbox(ty) => Op::Unbox(subst_all(ty, &self.margs, &self.rows)),
            op => op.clone(),
        }
    }

    pub fn block(&mut self, b: &Block, f: &mut Func) -> Block {
        let lets = b
            .lets
            .iter()
            .map(|l| Let {
                args: self.atoms(&l.args, f),
                binds: l.binds.iter().map(|&v| self.var(v, f)).collect(),
                op: self.op(&l.op),
                source: l.source.or(self.source),
            })
            .collect();
        let tail = match &b.tail {
            Tail::Join { rec, defs, body } => {
                // Labels first: a recursive definition can jump to itself.
                let labels: Vec<Label> = defs.iter().map(|d| self.label(d.label, f)).collect();
                Tail::Join {
                    rec: *rec,
                    defs: defs
                        .iter()
                        .zip(labels)
                        .map(|(d, label)| JoinDef {
                            label,
                            params: d.params.iter().map(|&v| self.var(v, f)).collect(),
                            body: self.block(&d.body, f),
                        })
                        .collect(),
                    body: Box::new(self.block(body, f)),
                }
            }
            Tail::Jump {
                target,
                args,
                source,
            } => Tail::Jump {
                target: self.label(*target, f),
                args: self.atoms(args, f),
                source: source.or(self.source),
            },
            Tail::Case {
                scrut,
                arms,
                default,
            } => Tail::Case {
                scrut: self.atom(scrut.clone(), f),
                arms: arms
                    .iter()
                    .map(|a| Arm {
                        tag: a.tag,
                        body: self.block(&a.body, f),
                    })
                    .collect(),
                default: default.as_deref().map(|d| Box::new(self.block(d, f))),
            },
            Tail::Unreachable => Tail::Unreachable,
            Tail::Trap => Tail::Trap,
        };
        Block { lets, tail }
    }
}
