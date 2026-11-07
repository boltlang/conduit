//! Dead code elimination: drops each `let` whose results are unused and
//! whose op [`is_removable`](conduit_ir::Effects::is_removable): it only
//! reads, and can't trap. It also drops each join definition nothing jumps
//! to. What an unused op allocates or changes, like `array_freeze`, nothing
//! else can see.
//!
//! Ops that can trap (signed division, float-to-integer conversions, array
//! accesses, `Unbox`) say so in their effects, so a trap is never removed.
//!
//! It works bottom-up in one pass. Variables are SSA, so whether a variable
//! is used is known once everything below its `let` has been seen. A join
//! definition's body is only looked at once something live jumps to it, so
//! uses in dead code never keep anything else alive, and a recursive join
//! point that only jumps to itself is dropped.

use conduit_ir::{Atom, Block, Func, FuncId, JoinDef, Label, Program, Tail, Var};

use crate::Pass;

/// The dead code elimination pass.
pub struct Dce;

impl Pass for Dce {
    fn name(&self) -> &'static str {
        "dce"
    }

    fn run(&self, _: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        dce(f)
    }
}

/// Removes dead code from `f`. Returns whether anything changed.
///
/// Variables and labels that are no longer bound stay in `f`'s tables.
pub fn dce(f: &mut Func) -> bool {
    let mut cx = Cx {
        used: vec![false; f.vars.len()],
        jumped_to: vec![false; f.labels.max(1) as usize],
        changed: false,
    };
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    f.body = cx.block(body);
    cx.changed
}

struct Cx {
    used: Vec<bool>,
    jumped_to: Vec<bool>,
    changed: bool,
}

impl Cx {
    fn use_atoms(&mut self, atoms: &[Atom]) {
        for a in atoms {
            if let Atom::Var(v) = a {
                self.used[v.0 as usize] = true;
            }
        }
    }

    fn is_used(&self, v: Var) -> bool {
        self.used[v.0 as usize]
    }

    fn is_jumped_to(&self, l: Label) -> bool {
        self.jumped_to[l.0 as usize]
    }

    fn block(&mut self, b: Block) -> Block {
        conduit_ir::deep(|| {
            // Bottom-up: the tail first, then the `let`s from the last one, so
            // each `let` is seen after everything that could use its results.
            // A join whose definitions are all dead is left without any: its
            // body then runs right after this block's `let`s, so it becomes
            // the rest of this block.
            let mut after = Vec::new();
            let tail = match self.tail(b.tail) {
                Tail::Join { defs, body, .. } if defs.is_empty() => {
                    let body = *body;
                    after = body.lets;
                    body.tail
                }
                tail => tail,
            };
            let mut lets = Vec::with_capacity(b.lets.len() + after.len());
            for l in b.lets.into_iter().rev() {
                let droppable = l.op.effects().is_removable();
                if droppable && !l.binds.iter().any(|&v| self.is_used(v)) {
                    self.changed = true;
                    continue;
                }
                self.use_atoms(&l.args);
                lets.push(l);
            }
            lets.reverse();
            lets.extend(after);
            Block { lets, tail }
        })
    }

    fn tail(&mut self, t: Tail) -> Tail {
        conduit_ir::deep(|| match t {
            Tail::Jump {
                target,
                args,
                source,
            } => {
                self.jumped_to[target.0 as usize] = true;
                self.use_atoms(&args);
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
            } => {
                let arms = arms
                    .into_iter()
                    .map(|mut arm| {
                        arm.body = self.block(arm.body);
                        arm
                    })
                    .collect();
                let default = default.map(|d| Box::new(self.block(*d)));
                self.use_atoms(std::slice::from_ref(&scrut));
                Tail::Case {
                    scrut,
                    arms,
                    default,
                }
            }
            Tail::Join { rec, defs, body } => self.join(rec, defs, *body),
            Tail::Unreachable => Tail::Unreachable,
            Tail::Trap => Tail::Trap,
        })
    }

    /// A join without live definitions is left without any, for
    /// [`Cx::block`] to replace with its body.
    fn join(&mut self, rec: bool, defs: Vec<JoinDef>, body: Block) -> Tail {
        conduit_ir::deep(|| {
            let body = self.block(body);
            let mut defs: Vec<(bool, JoinDef)> = defs.into_iter().map(|d| (false, d)).collect();
            // A definition is live once the body or a live definition jumps to
            // it. Only recursive definitions can jump to each other, so others
            // need one round.
            loop {
                let mut found = false;
                for (live, def) in &mut defs {
                    if !*live && self.is_jumped_to(def.label) {
                        *live = true;
                        found = true;
                        let body = std::mem::replace(&mut def.body, Tail::Unreachable.into());
                        def.body = self.block(body);
                    }
                }
                if !rec || !found {
                    break;
                }
            }
            let before = defs.len();
            let defs: Vec<JoinDef> = defs
                .into_iter()
                .filter_map(|(live, def)| live.then_some(def))
                .collect();
            if defs.len() != before {
                self.changed = true;
            }
            if defs.is_empty() {
                return Tail::Join {
                    rec,
                    defs,
                    body: Box::new(body),
                };
            }
            Tail::Join {
                rec,
                defs,
                body: Box::new(body),
            }
        })
    }
}
