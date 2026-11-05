//! Handlers their body can't use: a `handle H(env.., body)` whose body
//! performs nothing, by its row, can never run a clause of `H`, so it's
//! the body, then the return clause:
//!
//! ```text
//! let rs = handle H(env.., c)
//! ```
//!
//! becomes
//!
//! ```text
//! let xs = apply(c)
//! let rs = call ret(env.., xs)
//! ```
//!
//! where `ret` is `H`'s return clause, whose row is in the handler's, which
//! the `handle`'s function has. With the closure known, `closures` and
//! `inline` then make it direct.
//!
//! It's not enough that the body's row lacks `H`'s effect: a body that
//! performs some other effect `F` can still reach `H`, since an outer
//! clause for `F` may `discontinue` with `H`'s effect, which then performs
//! it where `F` was performed, inside the body, under `H`. A body that
//! performs nothing has nothing captured in it, so that can't happen.

use conduit_ir::{Atom, Block, Func, FuncId, Let, Op, Program, Tail, Ty};

use crate::Pass;

/// The pass that removes handlers their body can't use.
pub struct Handlers;

impl Pass for Handlers {
    fn name(&self) -> &'static str {
        "handlers"
    }

    fn run(&self, p: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        unused_handlers(p, f)
    }
}

/// Removes the handlers of `f`, a function of `p`, that their body can't
/// use. Returns whether anything changed.
pub fn unused_handlers(p: &dyn Program, f: &mut Func) -> bool {
    let mut body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    let changed = block(p, &mut body, f);
    f.body = body;
    changed
}

fn block(p: &dyn Program, b: &mut Block, f: &mut Func) -> bool {
    let mut changed = false;
    let mut lets = Vec::with_capacity(b.lets.len());
    for l in std::mem::take(&mut b.lets) {
        match unused(p, &l, f) {
            Some(ret) => {
                changed = true;
                let (c, env) = l.args.split_last().expect("a handle has a body");
                let Ty::Fn(sig, _) = f.var_ty(var(c)).clone() else {
                    unreachable!("checked by `unused`")
                };
                let xs: Vec<_> = sig.results.iter().map(|t| f.new_var(t.clone())).collect();
                lets.push(Let {
                    binds: xs.clone(),
                    op: Op::Apply,
                    args: vec![c.clone()],
                    source: l.source,
                });
                let mut args = env.to_vec();
                args.extend(xs.into_iter().map(Atom::Var));
                lets.push(Let {
                    binds: l.binds,
                    op: Op::Call(ret, Vec::new()),
                    args,
                    source: l.source,
                });
            }
            None => lets.push(l),
        }
    }
    b.lets = lets;
    match &mut b.tail {
        Tail::Join { defs, body, .. } => {
            for d in defs {
                changed |= block(p, &mut d.body, f);
            }
            changed |= block(p, body, f);
        }
        Tail::Case { arms, default, .. } => {
            for a in arms {
                changed |= block(p, &mut a.body, f);
            }
            if let Some(d) = default {
                changed |= block(p, d, f);
            }
        }
        Tail::Jump { .. } | Tail::Unreachable => {}
    }
    changed
}

/// The return clause of the handler `l` runs its body under, if `l` is a
/// `handle` whose body can't perform the handler's effect.
fn unused(p: &dyn Program, l: &Let, f: &Func) -> Option<FuncId> {
    let Op::Handle(h) = l.op else { return None };
    let decl = p.handler(h)?;
    let Some(Atom::Var(c)) = l.args.last() else {
        return None;
    };
    let Ty::Fn(sig, _) = f.var_ty(*c) else {
        return None;
    };
    sig.row.is_nothing().then_some(decl.ret)
}

fn var(a: &Atom) -> conduit_ir::Var {
    match a {
        Atom::Var(v) => *v,
        Atom::Lit(_) => unreachable!("a body is a closure"),
    }
}
