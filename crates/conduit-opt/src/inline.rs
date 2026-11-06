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

use conduit_ir::{Arm, Atom, Block, Func, FuncId, JoinDef, Label, Let, Op, Program, Tail};

use crate::copy::{Rename, has_op};
use crate::{MAX_CALLEE_SIZE, MAX_CALLER_SIZE, Pass};

/// The inlining pass.
pub struct Inline;

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
        size: body.size(),
        changed: false,
    };
    f.body = cx.block(body, f);
    cx.changed
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
        let n = callee.body.size();
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
        self.size += callee.body.size();
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
        let ret = direct.unwrap_or_else(|| f.new_label());
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
