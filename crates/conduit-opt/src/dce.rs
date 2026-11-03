//! Dead code elimination: drops each `let` whose results are unused and
//! whose op is [`Effects::Pure`], and each join definition nothing jumps
//! to.
//!
//! Ops that can trap (division, float-to-integer conversions, array
//! accesses, `Unbox`) have effect `Unknown`, so a trap is never removed.
//!
//! It works bottom-up in one pass. Variables are SSA, so whether a variable
//! is used is known once everything below its `let` has been seen. A join
//! definition's body is only looked at once something live jumps to it, so
//! uses in dead code never keep anything else alive, and a recursive join
//! point that only jumps to itself is dropped.

use conduit_ir::{Atom, Effects, Func, FuncId, JoinDef, Label, Op, Program, SourceId, Term, Var};

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
    let body = std::mem::replace(&mut f.body, Term::Unreachable);
    f.body = cx.term(body);
    cx.changed
}

struct Cx {
    used: Vec<bool>,
    jumped_to: Vec<bool>,
    changed: bool,
}

/// A `let` without its body.
struct Binding {
    binds: Vec<Var>,
    op: Op,
    args: Vec<Atom>,
    source: Option<SourceId>,
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

    fn term(&mut self, t: Term) -> Term {
        // Take the chain of `let`s apart without recursing, so a long
        // function doesn't overflow the stack, then put back the live ones
        // from the bottom up.
        let mut chain = Vec::new();
        let mut t = t;
        while let Term::Let {
            binds,
            op,
            args,
            body,
            source,
        } = t
        {
            chain.push(Binding {
                binds,
                op,
                args,
                source,
            });
            t = *body;
        }
        let mut t = self.tail(t);
        while let Some(b) = chain.pop() {
            if b.op.effects() == Effects::Pure && !b.binds.iter().any(|&v| self.is_used(v)) {
                self.changed = true;
                continue;
            }
            self.use_atoms(&b.args);
            t = Term::Let {
                binds: b.binds,
                op: b.op,
                args: b.args,
                body: Box::new(t),
                source: b.source,
            };
        }
        t
    }

    /// A term that isn't a `let`.
    fn tail(&mut self, t: Term) -> Term {
        match t {
            Term::Let { .. } => unreachable!("`term` takes `let`s apart"),
            Term::Jump {
                target,
                args,
                source,
            } => {
                self.jumped_to[target.0 as usize] = true;
                self.use_atoms(&args);
                Term::Jump {
                    target,
                    args,
                    source,
                }
            }
            Term::Case {
                scrut,
                arms,
                default,
            } => {
                let arms = arms
                    .into_iter()
                    .map(|mut arm| {
                        arm.body = self.term(arm.body);
                        arm
                    })
                    .collect();
                let default = default.map(|d| Box::new(self.term(*d)));
                self.use_atoms(std::slice::from_ref(&scrut));
                Term::Case {
                    scrut,
                    arms,
                    default,
                }
            }
            Term::Join { rec, defs, body } => self.join(rec, defs, *body),
            Term::Unreachable => Term::Unreachable,
        }
    }

    fn join(&mut self, rec: bool, defs: Vec<JoinDef>, body: Term) -> Term {
        let body = self.term(body);
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
                    let body = std::mem::replace(&mut def.body, Term::Unreachable);
                    def.body = self.term(body);
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
            return body;
        }
        Term::Join {
            rec,
            defs,
            body: Box::new(body),
        }
    }
}
