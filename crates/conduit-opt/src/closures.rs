//! Known closures: an `apply` of a closure made by `closure f[ms](..)` in
//! the same function is `call f[ms](captures.., args..)`, since a lifted
//! function takes its captures first. The call skips the closure's
//! wrapper, and inlining can then see it. If nothing else uses the
//! closure, dead code removes it.
//!
//! It works forward in one pass: variables are SSA, so a closure's `let` is
//! seen before every use of it. A closure that comes in as a parameter or
//! through a join point isn't known.

use std::collections::HashMap;

use conduit_ir::{Atom, Block, Func, FuncId, Mutability, Op, Program, Tail, Var};

use crate::Pass;

/// The known closures pass.
pub struct KnownClosures;

impl Pass for KnownClosures {
    fn name(&self) -> &'static str {
        "closures"
    }

    fn run(&self, _: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        known_closures(f)
    }
}

/// Turns each `apply` of a closure made in `f` into a call. Returns whether
/// anything changed.
pub fn known_closures(f: &mut Func) -> bool {
    let mut cx = Cx {
        closures: HashMap::new(),
        changed: false,
    };
    cx.block(&mut f.body);
    cx.changed
}

struct Cx {
    /// The closures made so far: their function, its mutability arguments
    /// and the captures.
    closures: HashMap<Var, (FuncId, Vec<Mutability>, Vec<Atom>)>,
    changed: bool,
}

impl Cx {
    fn block(&mut self, b: &mut Block) {
        for l in &mut b.lets {
            if l.op == Op::Apply
                && let Some(Atom::Var(c)) = l.args.first()
                && let Some((g, margs, captures)) = self.closures.get(c)
            {
                l.op = Op::Call(*g, margs.clone());
                l.args.splice(..1, captures.iter().cloned());
                self.changed = true;
            }
            if let (Op::Closure(g, margs), [v]) = (&l.op, &l.binds[..]) {
                self.closures
                    .insert(*v, (*g, margs.clone(), l.args.clone()));
            }
        }
        match &mut b.tail {
            Tail::Join { defs, body, .. } => {
                for d in defs {
                    self.block(&mut d.body);
                }
                self.block(body);
            }
            Tail::Case { arms, default, .. } => {
                for a in arms {
                    self.block(&mut a.body);
                }
                if let Some(d) = default {
                    self.block(d);
                }
            }
            Tail::Jump { .. } | Tail::Unreachable => {}
        }
    }
}
