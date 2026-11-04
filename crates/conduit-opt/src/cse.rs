//! Common subexpression elimination: a `let` with the same op and
//! arguments as one before it gives the same result, and is replaced by
//! that one's, when:
//!
//! - the op only computes its result, like `add`, or reads something that
//!   never changes: a field of an `Imm` object, an element of an `Imm`
//!   array, the length of any array, or what a box holds;
//! - or it reads something that can change, a `Mut` or `ReadOnly` object
//!   or a ref, and nothing between the two may write.
//!
//! An op that may trap can still be shared: if the first didn't trap, the
//! second won't. An op that makes a new object is only shared when nothing
//! can tell the two apart: an `Imm` object or array, a closure, or a box.
//! Never a ref, a `Mut` object, or the result of an op that runs code.
//!
//! The first `let` must come before the second on every path to it: in the
//! same block, or in one the second is nested in. A join definition can be
//! jumped to after anything, so reads of what can change aren't carried
//! into it.
//!
//! It works forward in one pass, as variables are SSA. A `let` that's
//! replaced leaves its variable unbound.

use std::collections::HashMap;

use conduit_ir::{Atom, Block, Func, FuncId, Let, Memory, Mutability, Op, Program, Tail, Ty, Var};

use crate::Pass;

/// The common subexpression elimination pass.
pub struct Cse;

impl Pass for Cse {
    fn name(&self) -> &'static str {
        "cse"
    }

    fn run(&self, _: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        cse(f)
    }
}

/// Replaces repeated ops in `f`. Returns whether anything changed.
pub fn cse(f: &mut Func) -> bool {
    let mut cx = Cx {
        vars: &f.vars,
        subst: HashMap::new(),
        fixed: HashMap::new(),
        changing: HashMap::new(),
        changed: false,
    };
    cx.block(&mut f.body);
    cx.changed
}

type Key = (Op, Vec<Atom>);

struct Cx<'f> {
    vars: &'f [Ty],
    /// The variables of `let`s that were replaced, and what they are.
    subst: HashMap<Var, Var>,
    /// Ops whose result can't change, and the variable that holds it.
    fixed: HashMap<Key, Var>,
    /// Reads of what can change, until something may write.
    changing: HashMap<Key, Var>,
    changed: bool,
}

/// Whether an op can be shared, and if so, whether its result can change
/// before it runs again.
enum Kind {
    Never,
    Fixed,
    Changing,
}

impl Cx<'_> {
    fn ty(&self, v: Var) -> &Ty {
        &self.vars[v.0 as usize]
    }

    /// Whether `a` is an object that can change after it's made.
    fn can_change(&self, a: &Atom) -> bool {
        match a {
            Atom::Var(v) => !matches!(
                self.ty(*v).mutability(),
                Option::None | Some(Mutability::Imm)
            ),
            Atom::Lit(_) => false,
        }
    }

    fn kind(&self, l: &Let) -> Kind {
        let [bind] = l.binds[..] else {
            return Kind::Never;
        };
        let imm = self.ty(bind).mutability() == Some(Mutability::Imm);
        let reads = |cx: &Self| match l.args.first() {
            Some(a) if cx.can_change(a) => Kind::Changing,
            _ => Kind::Fixed,
        };
        match &l.op {
            Op::Prim(_) | Op::Closure(..) | Op::Box(_) => Kind::Fixed,
            // An array's length never changes, and neither does a box.
            Op::ArrayLen | Op::Unbox(_) => Kind::Fixed,
            Op::New(_) | Op::ArrayNew | Op::ArrayFill if imm => Kind::Fixed,
            Op::ArrayClone if imm => reads(self),
            Op::Get(..) | Op::ArrayGet | Op::RefGet => reads(self),
            _ => Kind::Never,
        }
    }

    fn block(&mut self, b: &mut Block) {
        let (fixed, changing) = (self.fixed.clone(), self.changing.clone());
        let mut lets = Vec::with_capacity(b.lets.len());
        for mut l in std::mem::take(&mut b.lets) {
            self.atoms(&mut l.args);
            if self.let_(&l) {
                self.changed = true;
            } else {
                lets.push(l);
            }
        }
        b.lets = lets;
        match &mut b.tail {
            Tail::Jump { args, .. } => self.atoms(args),
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                self.atom(scrut);
                for a in arms {
                    self.block(&mut a.body);
                }
                if let Some(d) = default {
                    self.block(d);
                }
            }
            Tail::Join { defs, body, .. } => {
                for d in defs {
                    // Anything may have run before a jump to it.
                    let changing = std::mem::take(&mut self.changing);
                    self.block(&mut d.body);
                    self.changing = changing;
                }
                self.block(body);
            }
            Tail::Unreachable => {}
        }
        self.fixed = fixed;
        self.changing = changing;
    }

    /// Records what `l` gives, or replaces it with an earlier `let`'s
    /// variable. Returns whether it was replaced.
    fn let_(&mut self, l: &Let) -> bool {
        if l.op.effects().memory >= Memory::Writes {
            self.changing.clear();
        }
        let table = match self.kind(l) {
            Kind::Never => return false,
            Kind::Fixed => &mut self.fixed,
            Kind::Changing => &mut self.changing,
        };
        let bind = l.binds[0];
        let key = (l.op.clone(), l.args.clone());
        match table.get(&key) {
            Some(&earlier) if self.vars[earlier.0 as usize] == self.vars[bind.0 as usize] => {
                self.subst.insert(bind, earlier);
                true
            }
            Some(_) => false,
            None => {
                table.insert(key, bind);
                false
            }
        }
    }

    fn atom(&self, a: &mut Atom) {
        if let Atom::Var(v) = a
            && let Some(&to) = self.subst.get(v)
        {
            *a = Atom::Var(to);
        }
    }

    fn atoms(&self, atoms: &mut [Atom]) {
        atoms.iter_mut().for_each(|a| self.atom(a));
    }
}
