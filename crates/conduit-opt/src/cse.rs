//! Common subexpression elimination: a `let` with the same op and
//! arguments as one before it gives the same result, and is replaced by
//! that one's, when:
//!
//! - the op only computes its result, like `add`, or reads something that
//!   never changes: a constant, a field of an `Imm` object, an element of
//!   an `Imm` array, the length of any array, or what a box holds;
//! - or it reads something that can change, a `Mut` or `ReadOnly` object
//!   or a ref, and nothing between the two may write.
//!
//! An op that may trap can still be shared: if the first didn't trap, the
//! second won't. An op that makes a new object is only shared when nothing
//! can tell the two apart: an `Imm` object or array, a closure, or a box.
//! Never a ref, a `Mut` object, or the result of an op that runs code.
//!
//! A read of what was just written gives what was written: `get C.i(x)`
//! after `set C.i(x, v)` is `v`, and so are `ref_get(r)` after
//! `ref_set(r, v)` and `array_get(a, i)` after `array_set(a, i, v)` (if the
//! `set` didn't trap, the index is in bounds). So is a field of a `mut`
//! object right after its `new`, an element of a `mut` array right after
//! its `array_new`, or what a ref holds right after its `ref_new`. This
//! lasts, like a read, until something may write.
//!
//! An `apply` of an `imm` closure without effects, to arguments that are
//! `imm` all the way down, doesn't stop sharing reads: it can only reach
//! `imm` objects and the ones it makes, so it writes nothing that existed
//! before.
//!
//! A call of a function that only reads, with no effects and nothing
//! that writes in it or the functions it calls, doesn't stop sharing reads
//! either, and two of them with the same arguments give the same, if it
//! gives something that's `imm` all the way down. It may still trap or
//! not come back, so it's never removed. This looks at callees' bodies
//! through [`Program::func`], so a Salsa frontend records the dependency.
//!
//! A continuation is never reused: the verifier counts its uses along each
//! path, so reading one twice, or reading one back, would look like using
//! it twice, as `promote` keeps refs of them too.
//!
//! The first `let` must come before the second on every path to it: in the
//! same block, or in one the second is nested in. A join definition can be
//! jumped to after anything, so reads of what can change aren't carried
//! into it.
//!
//! It works forward in one pass, as variables are SSA. A `let` that's
//! replaced leaves its variable unbound.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use conduit_ir::types::deep_imm;
use conduit_ir::{
    Atom, Block, Func, FuncId, Let, Lit, Memory, Mutability, Op, Program, Tail, Ty, Var,
};

use crate::Pass;

/// The common subexpression elimination pass.
pub struct Cse;

impl Pass for Cse {
    fn name(&self) -> &'static str {
        "cse"
    }

    fn run(&self, p: &dyn Program, _: FuncId, f: &mut Func) -> bool {
        cse(p, f)
    }
}

/// Replaces repeated ops in `f`. Returns whether anything changed.
pub fn cse(p: &dyn Program, f: &mut Func) -> bool {
    let mut cx = Cx {
        p,
        vars: &f.vars,
        subst: HashMap::new(),
        fixed: HashMap::new(),
        fixed_order: Vec::new(),
        changing: HashMap::new(),
        read_only: RefCell::new(HashMap::new()),
        changed: false,
    };
    cx.block(&mut f.body);
    cx.changed
}

type Key = (Op, Vec<Atom>);

struct Cx<'f> {
    p: &'f dyn Program,
    vars: &'f [Ty],
    /// The variables of `let`s that were replaced, and what they are.
    subst: HashMap<Var, Atom>,
    /// Ops whose result can't change, and the variable that holds it.
    fixed: HashMap<Key, Var>,
    /// The keys of `fixed`, in the order they went in, so that a block
    /// can take out what it put in when it ends, which copying `fixed`
    /// for each block of a deep nest would make quadratic.
    fixed_order: Vec<Key>,
    /// Reads of what can change, and what they give, until something may
    /// write: an earlier read's variable, or what was written.
    changing: HashMap<Key, Atom>,
    /// The functions known to only read, or not to.
    read_only: RefCell<HashMap<FuncId, bool>>,
    changed: bool,
}

/// Whether an op can be shared, and if so, whether its result can change
/// before it runs again.
#[derive(Clone, Copy, PartialEq, Eq)]
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
            Op::Prim(_) | Op::Closure(..) | Op::Box(_) | Op::Const(_) => Kind::Fixed,
            // An array's length never changes, and neither does a box.
            Op::ArrayLen | Op::Unbox(_) => Kind::Fixed,
            Op::New(_) | Op::ArrayNew | Op::ArrayFill if imm => Kind::Fixed,
            Op::ArrayClone if imm => reads(self),
            Op::Get(..) | Op::ArrayGet | Op::ArrayLoad | Op::RefGet => reads(self),
            // A call that only reads gives the same again, unless it makes
            // something that can change, which two calls make two of.
            Op::Call(g, ..) if self.reads_only(*g) && deep_imm(self.p, self.ty(bind)) => {
                if l.args.iter().any(|a| self.can_change(a)) {
                    Kind::Changing
                } else {
                    Kind::Fixed
                }
            }
            _ => Kind::Never,
        }
    }

    fn block(&mut self, b: &mut Block) {
        conduit_ir::deep(|| {
            let (fixed, changing) = (self.fixed_order.len(), self.changing.clone());
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
                Tail::Unreachable | Tail::Trap => {}
            }
            for key in self.fixed_order.drain(fixed..) {
                self.fixed.remove(&key);
            }
            self.changing = changing;
        })
    }

    /// Records what `l` gives, or replaces it with an earlier `let`'s
    /// variable or what was written. Returns whether it was replaced.
    fn let_(&mut self, l: &Let) -> bool {
        let reads_only_call = matches!(&l.op, Op::Call(g, ..) if self.reads_only(*g));
        if l.op.effects().memory >= Memory::Writes && !self.imm_apply(l) && !reads_only_call {
            self.changing.clear();
        }
        self.written(l);
        let kind = self.kind(l);
        if kind == Kind::Never {
            return false;
        }
        let bind = l.binds[0];
        // The verifier counts the uses of a continuation along each path,
        // so one read twice, or read back, would look like it's used
        // twice.
        if matches!(self.ty(bind), Ty::Cont(_)) {
            return false;
        }
        let key = (l.op.clone(), l.args.clone());
        let known = match kind {
            Kind::Fixed => self.fixed.get(&key).map(|&v| Atom::Var(v)),
            _ => self.changing.get(&key).cloned(),
        };
        match known {
            Some(earlier) if self.atom_ty(&earlier) == *self.ty(bind) => {
                self.subst.insert(bind, earlier);
                true
            }
            Some(_) => false,
            None => {
                match kind {
                    Kind::Fixed => {
                        self.fixed_order.push(key.clone());
                        self.fixed.insert(key, bind);
                    }
                    _ => {
                        self.changing.insert(key, Atom::Var(bind));
                    }
                }
                false
            }
        }
    }

    /// Whether `g` only reads: it has no effects, its ops at most read,
    /// and so do the functions it calls. It may still trap or not come
    /// back.
    fn reads_only(&self, g: FuncId) -> bool {
        if let Some(&known) = self.read_only.borrow().get(&g) {
            return known;
        }
        // Every function `g` reaches through calls, each looked at once,
        // so that recursion ends. Only if none writes is any of them known
        // to only read: until then, one of them might reach a write.
        let mut seen = BTreeSet::from([g]);
        let mut todo = vec![g];
        let mut ok = true;
        while let Some(h) = todo.pop() {
            let Some(f) = self.p.func(h) else {
                ok = false;
                break;
            };
            ok = f.row.is_nothing()
                && lets_all(&f.body, &mut |l| match &l.op {
                    Op::Call(next, ..) => {
                        if seen.insert(*next) {
                            todo.push(*next);
                        }
                        true
                    }
                    op => op.effects().memory <= Memory::Reads,
                });
            if !ok {
                break;
            }
        }
        let mut known = self.read_only.borrow_mut();
        if ok {
            known.extend(seen.into_iter().map(|h| (h, true)));
        } else {
            known.insert(g, false);
        }
        ok
    }

    /// Whether `l` applies an `imm` closure without effects to arguments
    /// that are `imm` all the way down. It only reaches `imm` objects and
    /// the ones it makes, so it writes nothing that existed before.
    fn imm_apply(&self, l: &Let) -> bool {
        let Some((Atom::Var(c), rest)) = l.args.split_first() else {
            return false;
        };
        let Ty::Fn(sig, Mutability::Imm) = self.ty(*c) else {
            return false;
        };
        l.op == Op::Apply
            && sig.row.is_nothing()
            && rest.iter().all(|a| deep_imm(self.p, &self.atom_ty(a)))
    }

    /// Records what reading back what `l` writes, or what a `mut` object
    /// it makes holds, gives.
    fn written(&mut self, l: &Let) {
        let fresh_mut = match &l.binds[..] {
            [x] => self.can_change(&Atom::Var(*x)),
            _ => false,
        };
        let mut put = |op: Op, args: Vec<Atom>, value: &Atom| {
            self.changing.insert((op, args), value.clone());
        };
        match (&l.op, &l.args[..], &l.binds[..]) {
            (Op::Set(c, i), [x, v], []) => put(Op::Get(*c, *i), vec![x.clone()], v),
            (Op::RefSet, [r, v], []) => put(Op::RefGet, vec![r.clone()], v),
            (Op::ArraySet, [a, i, v], []) => put(Op::ArrayGet, vec![a.clone(), i.clone()], v),
            // Read back as the same type only: a load of another type
            // at the same offset doesn't match, since its variable's type
            // differs.
            (Op::ArrayStore, [a, at, v], []) => put(Op::ArrayLoad, vec![a.clone(), at.clone()], v),
            (Op::RefNew, [v], [r]) => put(Op::RefGet, vec![Atom::Var(*r)], v),
            (Op::New(c), fields, [x]) if fresh_mut => {
                for (i, v) in fields.iter().enumerate() {
                    put(Op::Get(*c, i as u32), vec![Atom::Var(*x)], v);
                }
            }
            (Op::ArrayNew, elems, [a]) if fresh_mut => {
                for (i, v) in elems.iter().enumerate() {
                    let index = Atom::Lit(Lit::I32(i as i32));
                    put(Op::ArrayGet, vec![Atom::Var(*a), index], v);
                }
            }
            _ => {}
        }
    }

    fn atom_ty(&self, a: &Atom) -> Ty {
        match a {
            Atom::Var(v) => self.ty(*v).clone(),
            Atom::Lit(lit) => lit.ty(),
        }
    }

    fn atom(&self, a: &mut Atom) {
        if let Atom::Var(v) = a
            && let Some(to) = self.subst.get(v)
        {
            *a = to.clone();
        }
    }

    fn atoms(&self, atoms: &mut [Atom]) {
        atoms.iter_mut().for_each(|a| self.atom(a));
    }
}

/// Whether `test` holds for every `let` in `b`, nested ones included.
fn lets_all(b: &Block, test: &mut impl FnMut(&Let) -> bool) -> bool {
    b.all_lets().all(test)
}
