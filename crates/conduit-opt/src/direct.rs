//! Handlers run directly: a `handle` whose body is a known closure that
//! performs the handled effect only itself, with clauses that are all
//! tail-resumptive or aborting, becomes the body's code, with each
//! `perform` replaced by its clause (issue #24):
//!
//! ```text
//! let rs = handle H(env.., c)          join after(rs..) = REST
//! REST                          =>     in join done(xs..) =
//!                                        let rs' = call H.ret(env.., xs..)
//!                                        jump after(rs')
//!                                      in BODY
//! ```
//!
//! `BODY` is the body's function copied as `inline` copies a callee, with
//! its `ret` as `done`. Each `let xs = perform E.i(args..)` in it, with
//! `rest` what follows, becomes a copy of clause `i` without its
//! continuation: a tail-resumptive one's `ret`, where it would resume,
//! is a join point `k(xs..) = rest`, and an aborting one's `ret` is
//! `after`, skipping `rest` and the return clause. The copies run where
//! the `handle` was, under the handlers outside it, as clauses do.
//!
//! When it applies:
//!
//! - **The body is a known closure**, made by `closure f(caps..)` in the
//!   same function.
//! - **It performs only the handled effect, and only itself:** every other
//!   op of `f` has an empty row, so no call, `apply`, inner `handle`,
//!   `resume` or `discontinue` in it can perform anything. That also rules
//!   out an effect injected into the body by a `discontinue` from outside,
//!   which needs the body to perform something else.
//! - **Every clause is tail-resumptive without `discontinue`, or
//!   aborting** (see [`conduit_ir::clause`]).
//! - **They're small**, as `inline` wants callees, since each `perform`
//!   gets a copy of its clause, and the handler has no effect parameters.
//!
//! `promote`, `joins` and `simplify` then see ordinary code. A `perform` in
//! a callee too big to inline still needs a real handler.

use std::collections::BTreeMap;

use conduit_ir::clause::{aborting, discontinues, plain_call};
use conduit_ir::types::subst_all;
use conduit_ir::{
    Atom, Block, EffectId, Func, FuncId, JoinDef, Label, Let, Mutability, Op, Program, Row, Tail,
    Var,
};

use crate::Pass;
use crate::copy::{Rename, has_op};
use crate::{MAX_CALLEE_SIZE, MAX_CALLER_SIZE};

/// The pass that runs handlers directly.
pub struct Direct;

impl Pass for Direct {
    fn name(&self) -> &'static str {
        "direct"
    }

    fn run(&self, p: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        direct(p, f)
    }
}

/// Runs the handlers of `f`, a function of `p`, directly where it can.
/// Returns whether anything changed.
pub fn direct(p: &dyn Program, f: &mut Func) -> bool {
    let mut closures = BTreeMap::new();
    let mut todo = vec![&f.body];
    while let Some(b) = todo.pop() {
        for l in &b.lets {
            if let (Op::Closure(g, ms, rs), [c]) = (&l.op, &l.binds[..]) {
                closures.insert(*c, (*g, ms.clone(), rs.clone(), l.args.clone()));
            }
        }
        todo.extend(b.children());
    }
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    let mut cx = Cx {
        p,
        closures,
        size: body.size(),
        changed: false,
    };
    f.body = cx.block(body, f);
    cx.changed
}

/// A closure: its function, mutability arguments, rows and captures.
type Closure = (FuncId, Vec<Mutability>, Vec<Row>, Vec<Atom>);

/// A clause without its continuation.
enum Clause {
    /// Tail-resumptive: its `ret` gives what the `perform` gives.
    Resumes(Func),
    /// Aborting: its `ret` gives what the `handle` gives.
    Aborts(Func),
}

/// What a `handle` that can run directly needs.
struct Handled<'p> {
    effect: EffectId,
    ret: FuncId,
    body: &'p Func,
    closure: Closure,
    clauses: Vec<Clause>,
}

struct Cx<'p> {
    p: &'p dyn Program,
    closures: BTreeMap<Var, Closure>,
    /// The size of the function so far.
    size: usize,
    changed: bool,
}

impl<'p> Cx<'p> {
    /// What `l` needs to run directly, if it's a `handle` that can.
    fn handled(&self, l: &Let) -> Option<Handled<'p>> {
        let Op::Handle(h, rows) = &l.op else {
            return None;
        };
        let decl = self.p.handler(*h)?;
        if !rows.is_empty() || decl.effect_params != 0 {
            return None;
        }
        let Some(Atom::Var(c)) = l.args.last() else {
            return None;
        };
        let closure = self.closures.get(c)?.clone();
        let body = self.p.func(closure.0)?;
        if body.params.len() != closure.3.len()
            || body.body.size() > MAX_CALLEE_SIZE
            || (body.mut_params.count > 0 && has_op(&body.body, &|op| matches!(op, Op::Box(_))))
        {
            return None;
        }
        // Only the handled effect, and only performed by the body itself.
        let mut ok = true;
        let mut todo = vec![&body.body];
        while let Some(b) = todo.pop() {
            for l in &b.lets {
                ok &= match l.op {
                    Op::Perform(e, _) => e == decl.effect,
                    _ => l.row(self.p, body).is_nothing(),
                };
            }
            todo.extend(b.children());
        }
        if !ok {
            return None;
        }
        let mut clauses = Vec::new();
        let mut copies = body.body.size();
        for &c in &decl.ops {
            let cf = self.p.func(c)?;
            let clause = match plain_call(cf) {
                Some(plain) if !discontinues(&plain) => Clause::Resumes(plain),
                _ => Clause::Aborts(aborting(cf)?),
            };
            let (Clause::Resumes(cf) | Clause::Aborts(cf)) = &clause;
            if cf.body.size() > MAX_CALLEE_SIZE {
                return None;
            }
            copies += cf.body.size();
            clauses.push(clause);
        }
        if self.size + copies > MAX_CALLER_SIZE {
            return None;
        }
        Some(Handled {
            effect: decl.effect,
            ret: decl.ret,
            body,
            closure,
            clauses,
        })
    }

    fn block(&mut self, b: Block, f: &mut Func) -> Block {
        let mut lets = Vec::with_capacity(b.lets.len());
        let mut rest = b.lets.into_iter();
        while let Some(l) = rest.next() {
            let Some(handled) = self.handled(&l) else {
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
            let direct = self.run_directly(handled, l, after, f);
            lets.extend(direct.lets);
            return Block {
                lets,
                tail: direct.tail,
            };
        }
        let tail = match b.tail {
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
                    .map(|mut a| {
                        a.body = self.block(a.body, f);
                        a
                    })
                    .collect(),
                default: default.map(|d| Box::new(self.block(*d, f))),
            },
            t => t,
        };
        Block { lets, tail }
    }

    /// The block that replaces `handle`, of what `h` describes, and `after`,
    /// the rest of the block it's in.
    fn run_directly(&mut self, h: Handled<'_>, handle: Let, after: Block, f: &mut Func) -> Block {
        self.changed = true;
        let (_, margs, rows, caps) = h.closure;
        let env = handle.args[..handle.args.len() - 1].to_vec();
        let rs_args: Vec<Atom> = handle.binds.iter().map(|&v| Atom::Var(v)).collect();
        // What follows the `handle`: where its results go.
        let direct = match &after.tail {
            Tail::Jump { target, args, .. } if after.lets.is_empty() && *args == rs_args => {
                Some(*target)
            }
            _ => None,
        };
        let after_label = direct.unwrap_or_else(|| f.new_label());
        // `done(xs..)`: the body returned, so the return clause runs.
        let done = f.new_label();
        let xs: Vec<Var> = (h.body.results.iter())
            .map(|t| f.new_var(subst_all(t, &margs, &rows)))
            .collect();
        let rs2: Vec<Var> = (handle.binds.iter())
            .map(|v| f.new_var(f.var_ty(*v).clone()))
            .collect();
        let mut ret_args = env.clone();
        ret_args.extend(xs.iter().map(|&v| Atom::Var(v)));
        let done_body = Block {
            lets: vec![Let {
                binds: rs2.clone(),
                op: Op::Call(h.ret, Vec::new(), Vec::new()),
                args: ret_args,
                source: handle.source,
            }],
            tail: Tail::Jump {
                target: after_label,
                args: rs2.into_iter().map(Atom::Var).collect(),
                source: handle.source,
            },
        };
        let mut r = Rename {
            vars: h.body.params.iter().copied().zip(caps).collect(),
            labels: BTreeMap::from([(Label::RET, done)]),
            margs,
            rows,
            source: handle.source,
            callee: h.body,
        };
        let body = r.block(&h.body.body, f);
        self.size += body.size();
        let mut clauses = Clauses {
            effect: h.effect,
            clauses: &h.clauses,
            env: &env,
            after: after_label,
            source: handle.source,
            size: 0,
        };
        let body = clauses.block(body, f);
        self.size += clauses.size;
        let inner: Block = Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: done,
                params: xs,
                body: done_body,
            }],
            body: Box::new(body),
        }
        .into();
        if direct.is_some() {
            return inner;
        }
        Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: after_label,
                params: handle.binds,
                body: after,
            }],
            body: Box::new(inner),
        }
        .into()
    }
}

/// Replaces the `perform`s of a copied body with copies of their clauses.
struct Clauses<'a> {
    effect: EffectId,
    clauses: &'a [Clause],
    env: &'a [Atom],
    /// Where the `handle`'s results go, which an aborting clause gives.
    after: Label,
    source: Option<conduit_ir::SourceId>,
    size: usize,
}

impl Clauses<'_> {
    fn block(&mut self, b: Block, f: &mut Func) -> Block {
        let mut lets = Vec::with_capacity(b.lets.len());
        let mut rest = b.lets.into_iter();
        while let Some(l) = rest.next() {
            let Op::Perform(e, i) = l.op else {
                lets.push(l);
                continue;
            };
            debug_assert_eq!(e, self.effect, "the body performs only the handled effect");
            let after = self.block(
                Block {
                    lets: rest.collect(),
                    tail: b.tail,
                },
                f,
            );
            let copy = self.clause(i, l, after, f);
            lets.extend(copy.lets);
            return Block {
                lets,
                tail: copy.tail,
            };
        }
        let tail = match b.tail {
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
                    .map(|mut a| {
                        a.body = self.block(a.body, f);
                        a
                    })
                    .collect(),
                default: default.map(|d| Box::new(self.block(*d, f))),
            },
            t => t,
        };
        Block { lets, tail }
    }

    /// A copy of clause `i` for `perform`, with `rest` what follows it.
    fn clause(&mut self, i: u32, perform: Let, rest: Block, f: &mut Func) -> Block {
        let mut args = self.env.to_vec();
        args.extend(perform.args.iter().cloned());
        let (cf, resumes) = match &self.clauses[i as usize] {
            Clause::Resumes(cf) => (cf, true),
            Clause::Aborts(cf) => (cf, false),
        };
        self.size += cf.body.size();
        let xs: Vec<Atom> = perform.binds.iter().map(|&v| Atom::Var(v)).collect();
        // A resuming clause gives what the `perform` gives, to the rest; an
        // aborting one what the `handle` gives.
        let direct = match &rest.tail {
            Tail::Jump { target, args, .. } if resumes && rest.lets.is_empty() && *args == xs => {
                Some(*target)
            }
            _ => None,
        };
        let ret = if resumes {
            direct.unwrap_or_else(|| f.new_label())
        } else {
            self.after
        };
        let mut r = Rename {
            vars: cf.params.iter().copied().zip(args).collect(),
            labels: BTreeMap::from([(Label::RET, ret)]),
            margs: Vec::new(),
            rows: Vec::new(),
            source: perform.source.or(self.source),
            callee: cf,
        };
        let copy = r.block(&cf.body, f);
        if !resumes || direct.is_some() {
            return copy;
        }
        Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: ret,
                params: perform.binds,
                body: rest,
            }],
            body: Box::new(copy),
        }
        .into()
    }
}
