//! Known closures: an `apply` of a closure made by `closure f[ms](..)` in
//! the same function is `call f[ms](captures.., args..)`, since a lifted
//! function takes its captures first. The call skips the closure's
//! wrapper, and inlining can then see it. If nothing else uses the
//! closure, dead code removes it.
//!
//! It works forward in one pass: variables are SSA, so a closure's `let` is
//! seen before every use of it. A closure that comes in as a parameter or
//! through a join point isn't known.
//!
//! Nor is one that captures a continuation: the verifier counts the uses
//! of a continuation along each path, and leaves those through a closure to
//! run time, so two calls passing it would look like two uses of it.

use std::collections::HashMap;

use conduit_ir::{Atom, Block, Func, FuncId, Mutability, Op, Program, Row, Ty, Var};

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
        vars: &f.vars,
        closures: HashMap::new(),
        changed: false,
    };
    cx.block(&mut f.body);
    cx.changed
}

/// A closure made by `closure f[margs, rows](captures..)`.
struct Made {
    func: FuncId,
    margs: Vec<Mutability>,
    rows: Vec<Row>,
    captures: Vec<Atom>,
}

struct Cx<'f> {
    vars: &'f [Ty],
    /// The closures made so far.
    closures: HashMap<Var, Made>,
    changed: bool,
}

impl Cx<'_> {
    fn block(&mut self, b: &mut Block) {
        conduit_ir::deep(|| {
            for l in &mut b.lets {
                if l.op == Op::Apply
                    && let Some(Atom::Var(c)) = l.args.first()
                    && let Some(made) = self.closures.get(c)
                {
                    l.op = Op::Call(made.func, made.margs.clone(), made.rows.clone());
                    l.args.splice(..1, made.captures.iter().cloned());
                    self.changed = true;
                }
                let is_cont = |v: Var| matches!(self.vars.get(v.0 as usize), Some(Ty::Cont(_)));
                let captures_cont = l.args.iter().filter_map(Atom::var).any(is_cont);
                if let (Op::Closure(g, margs, rows), [v]) = (&l.op, &l.binds[..])
                    && !captures_cont
                {
                    let made = Made {
                        func: *g,
                        margs: margs.clone(),
                        rows: rows.clone(),
                        captures: l.args.clone(),
                    };
                    self.closures.insert(*v, made);
                }
            }
            // A closure's `let` comes before its uses, which are in this
            // block or in the blocks it ends in.
            for child in b.children_mut() {
                self.block(child);
            }
        })
    }
}
