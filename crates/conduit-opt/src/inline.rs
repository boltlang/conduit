//! Inlining: replaces `let rs = call g(args)` with the body of `g`.
//!
//! The callee's parameters become the arguments, its other variables and
//! its labels get fresh ones in the caller, and its mutability parameters
//! become the call's mutability arguments. Its `ret` becomes a fresh join
//! point whose parameters are `rs` and whose body is the rest of the
//! caller's block. When the rest is only `jump j(rs)`, as after a tail call,
//! the callee's `ret` is `j` itself, so no join point is needed and a tail
//! call in the callee stays one.
//!
//! A `let` or `jump` of the callee keeps its source, or gets the call's if
//! it has none, and its variables and labels keep their names.
//!
//! Only small callees are inlined, never the function itself, nor one that
//! calls itself, directly or through one other function, which would unroll
//! it a level each run. And only while the caller is below a size limit,
//! so that longer cycles of calls can't make it grow without end.
//!
//! Nor is a callee with mutability parameters that boxes: `box` of a type
//! with a parameter gives a `mut any`, and with `imm` in place, an
//! `imm any`, which doesn't fit the variable it binds. The code
//! that was inlined is looked at again only in the next run, so each run
//! inlines one level of calls.

use std::collections::BTreeMap;

use conduit_ir::verify::subst_all;
use conduit_ir::{
    Arm, Atom, Block, Func, FuncId, JoinDef, Label, Let, Mutability, Op, Program, Row, SourceId,
    Tail, Var,
};

use crate::Pass;

/// The inlining pass.
pub struct Inline;

/// The largest callee inlined, in `let`s and tails.
pub const MAX_CALLEE_SIZE: usize = 20;

/// Inlining stops once the caller is this big, in `let`s and tails.
pub const MAX_CALLER_SIZE: usize = 1000;

impl Pass for Inline {
    fn name(&self) -> &'static str {
        "inline"
    }

    fn run(&self, p: &dyn Program, id: FuncId, f: &mut Func) -> bool {
        inline(p, id, f)
    }
}

/// Inlines the calls in `f`, the function `id` of `p`, that are worth it.
/// Returns whether anything changed.
pub fn inline(p: &dyn Program, id: FuncId, f: &mut Func) -> bool {
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    let mut cx = Cx {
        p,
        id,
        size: size(&body),
        changed: false,
    };
    f.body = cx.block(body, f);
    cx.changed
}

/// How big a block is: its `let`s and tails, nested ones included.
pub(crate) fn size(b: &Block) -> usize {
    b.lets.len()
        + 1
        + match &b.tail {
            Tail::Join { defs, body, .. } => {
                defs.iter().map(|d| size(&d.body)).sum::<usize>() + size(body)
            }
            Tail::Case { arms, default, .. } => {
                arms.iter().map(|a| size(&a.body)).sum::<usize>()
                    + default.as_deref().map_or(0, size)
            }
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => 0,
        }
}

/// Whether some op in `b` is one that `is` holds for.
pub(crate) fn has_op(b: &Block, is: &dyn Fn(&Op) -> bool) -> bool {
    b.lets.iter().any(|l| is(&l.op))
        || match &b.tail {
            Tail::Join { defs, body, .. } => {
                defs.iter().any(|d| has_op(&d.body, is)) || has_op(body, is)
            }
            Tail::Case { arms, default, .. } => {
                arms.iter().any(|a| has_op(&a.body, is))
                    || default.as_deref().is_some_and(|d| has_op(d, is))
            }
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => false,
        }
}

struct Cx<'p> {
    p: &'p dyn Program,
    id: FuncId,
    /// The size of the caller so far.
    size: usize,
    changed: bool,
}

impl<'p> Cx<'p> {
    /// The callee of `l`, if it's a call worth inlining.
    fn callee(&self, l: &Let) -> Option<&'p Func> {
        let Op::Call(g, ..) = l.op else {
            return None;
        };
        if g == self.id {
            return None;
        }
        let callee = self.p.func(g)?;
        let n = size(&callee.body);
        let ok = n <= MAX_CALLEE_SIZE
            && self.size + n <= MAX_CALLER_SIZE
            && !self.recursive(g, callee)
            && !(callee.mut_params.count > 0
                && has_op(&callee.body, &|op| matches!(op, Op::Box(_))));
        ok.then_some(callee)
    }

    /// Whether `g` calls itself, or a function that calls `g`.
    fn recursive(&self, g: FuncId, callee: &Func) -> bool {
        let calls = |b: &Block| has_op(b, &|op| matches!(op, Op::Call(h, ..) if *h == g));
        has_op(&callee.body, &|op| match op {
            Op::Call(h, ..) => *h == g || self.p.func(*h).is_some_and(|hf| calls(&hf.body)),
            _ => false,
        })
    }

    fn block(&mut self, b: Block, f: &mut Func) -> Block {
        let mut lets = Vec::with_capacity(b.lets.len());
        let mut rest = b.lets.into_iter();
        while let Some(l) = rest.next() {
            let Some(callee) = self.callee(&l) else {
                lets.push(l);
                continue;
            };
            let after = self.block(
                Block {
                    lets: rest.collect(),
                    tail: b.tail,
                },
                f,
            );
            let inlined = self.inline_call(callee, l, after, f);
            lets.extend(inlined.lets);
            return Block {
                lets,
                tail: inlined.tail,
            };
        }
        Block {
            lets,
            tail: self.tail(b.tail, f),
        }
    }

    fn tail(&mut self, t: Tail, f: &mut Func) -> Tail {
        match t {
            Tail::Join { rec, defs, body } => Tail::Join {
                rec,
                defs: defs
                    .into_iter()
                    .map(|d| JoinDef {
                        body: self.block(d.body, f),
                        ..d
                    })
                    .collect(),
                body: Box::new(self.block(*body, f)),
            },
            Tail::Case {
                scrut,
                arms,
                default,
            } => Tail::Case {
                scrut,
                arms: arms
                    .into_iter()
                    .map(|a| Arm {
                        body: self.block(a.body, f),
                        ..a
                    })
                    .collect(),
                default: default.map(|d| Box::new(self.block(*d, f))),
            },
            t @ (Tail::Jump { .. } | Tail::Unreachable | Tail::Trap) => t,
        }
    }

    /// The block that replaces `call`, of `callee`, and `after`, the rest
    /// of the block it's in.
    fn inline_call(&mut self, callee: &Func, call: Let, after: Block, f: &mut Func) -> Block {
        self.changed = true;
        self.size += size(&callee.body);
        let Op::Call(_, margs, rows) = call.op else {
            unreachable!("only calls are inlined")
        };
        let binds_args: Vec<Atom> = call.binds.iter().map(|&b| Atom::Var(b)).collect();
        // After a tail call, or anything else that only passes the results
        // on, the callee can jump where they go.
        let direct = match &after.tail {
            Tail::Jump { target, args, .. } if after.lets.is_empty() && *args == binds_args => {
                Some(*target)
            }
            _ => None,
        };
        let ret = direct.unwrap_or_else(|| new_label(f));
        let mut r = Rename {
            vars: callee
                .params
                .iter()
                .copied()
                .zip(call.args.iter().cloned())
                .collect(),
            labels: BTreeMap::from([(Label::RET, ret)]),
            margs,
            rows,
            source: call.source,
            callee,
        };
        let body = r.block(&callee.body, f);
        if direct.is_some() {
            return body;
        }
        Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: ret,
                params: call.binds,
                body: after,
            }],
            body: Box::new(body),
        }
        .into()
    }
}

pub(crate) fn new_label(f: &mut Func) -> Label {
    f.labels += 1;
    Label(f.labels - 1)
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
        let new = new_label(f);
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
