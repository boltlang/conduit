//! Borrowed parameters: which a function only reads, so that its callers
//! keep the reference. See [`borrows`].

use super::*;

/// Which parameters of each function are borrowed: the caller keeps the
/// reference it passes, so neither side counts it. It's a convention
/// between a function and its callers, which [`borrows`] infers, and which
/// a frontend can give instead, for example to fix it in a function's
/// signature. Parameters it doesn't know are owned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Borrows {
    pub params: BTreeMap<FuncId, Vec<bool>>,
}

impl Borrows {
    pub fn is_borrowed(&self, f: FuncId, i: usize) -> bool {
        self.params
            .get(&f)
            .and_then(|ps| ps.get(i))
            .copied()
            .unwrap_or(false)
    }
}

/// Infers which parameters are borrowed, as Lean does: an object parameter
/// is borrowed unless the function gives its reference away, by passing it
/// to a use that takes it (a jump, a field, an owned parameter of a call,
/// ...). It's a fixpoint over the module, since a parameter passed on to
/// another function's is borrowed only if that one is.
///
/// Two more rules keep parameters owned:
///
/// - A function that's made into a closure, is a handler's clause, or is
///   exported, keeps every parameter owned: `apply` and `perform` don't
///   know whom they call, and a caller from outside the module isn't
///   counted with the module, so they always give their arguments away.
/// - A recursive tail call keeps being one: a call `f(.., y, ..)` whose
///   results the caller returns, where `f` can call the caller back, makes
///   `f`'s parameter owned if `y` is, since otherwise the caller would drop
///   `y` after the call, and a loop written as tail calls would grow the
///   stack. Other tail calls can lose that: one frame more is cheaper than
///   counting in every call of `f`.
pub fn borrows(m: &Module) -> Borrows {
    let mut always_owned: BTreeSet<FuncId> = BTreeSet::new();
    for f in m.funcs.values() {
        each_let(&f.body, &mut |l| {
            if let Op::Closure(g, ..) = l.op {
                always_owned.insert(g);
            }
        });
    }
    for h in m.handlers.values() {
        always_owned.extend(h.clauses().map(|(_, _, c)| c));
        always_owned.insert(h.ret);
    }
    always_owned.extend(m.exports.keys());
    let reaches = reaches(m);
    let mut b = Borrows::default();
    for (id, f) in &m.funcs {
        let borrowed = f
            .params
            .iter()
            .map(|p| f.var_ty(*p).is_object() && !always_owned.contains(id))
            .collect();
        b.params.insert(*id, borrowed);
    }
    loop {
        let mut owned: Vec<(FuncId, usize)> = Vec::new();
        for (id, f) in &m.funcs {
            let index = |v: &Var| f.params.iter().position(|p| p == v);
            let all_lent = lent_vars(*id, f, &b);
            let lent = |v: &Var| index(v).is_some_and(|i| b.is_borrowed(*id, i));
            let give = |a: &Atom, owned: &mut Vec<(FuncId, usize)>| {
                if let Atom::Var(v) = a
                    && lent(v)
                {
                    owned.push((*id, index(v).unwrap()));
                }
            };
            let mut todo = vec![&f.body];
            while let Some(block) = todo.pop() {
                for (n, l) in block.lets.iter().enumerate() {
                    for (i, a) in l.args.iter().enumerate() {
                        if takes_with(&b, &l.op, i) {
                            give(a, &mut owned);
                        }
                    }
                    let last = n + 1 == block.lets.len();
                    if let Op::Call(g, ..) = l.op
                        && last
                        && returns(&block.tail, &l.binds)
                        && reaches[&g].contains(id)
                    {
                        for (i, a) in l.args.iter().enumerate() {
                            let Atom::Var(y) = a else { continue };
                            if b.is_borrowed(g, i) && !all_lent.contains(y) {
                                owned.push((g, i));
                            }
                        }
                    }
                }
                if let Tail::Jump { args, .. } = &block.tail {
                    for a in args {
                        give(a, &mut owned);
                    }
                }
                todo.extend(block.children());
            }
        }
        let mut changed = false;
        for (f, i) in owned {
            if let Some(p) = b.params.get_mut(&f).and_then(|ps| ps.get_mut(i))
                && *p
            {
                *p = false;
                changed = true;
            }
        }
        if !changed {
            return b;
        }
    }
}

/// The variables of function `id` that are borrowed: its borrowed
/// parameters, and what's read out of a borrowed `imm` object, which the
/// object keeps alive, and unchanged, for as long as the caller does.
pub(super) fn lent_vars(id: FuncId, f: &Func, b: &Borrows) -> Set {
    let mut lent: Set = (f.params.iter().enumerate())
        .filter(|(i, _)| b.is_borrowed(id, *i))
        .map(|(_, p)| *p)
        .collect();
    // Variables are bound before they're used, and outer blocks before
    // inner ones, so one pass in that order finds them all.
    let mut todo = vec![&f.body];
    while let Some(block) = todo.pop() {
        for l in &block.lets {
            let reads = matches!(l.op, Op::Get(..) | Op::ArrayGet | Op::Unbox(_));
            if let ([Atom::Var(x), ..], [y]) = (&l.args[..], &l.binds[..])
                && reads
                && lent.contains(x)
                && f.var_ty(*x).mutability() == Some(Mutability::Imm)
                && f.var_ty(*y).is_object()
            {
                lent.insert(*y);
            }
        }
        todo.extend(block.children());
    }
    lent
}

/// The functions each function can call, directly or not.
pub(super) fn reaches(m: &Module) -> BTreeMap<FuncId, BTreeSet<FuncId>> {
    let calls: BTreeMap<FuncId, BTreeSet<FuncId>> = m
        .funcs
        .iter()
        .map(|(id, f)| {
            let mut callees = BTreeSet::new();
            each_let(&f.body, &mut |l| {
                if let Op::Call(g, ..) = l.op {
                    callees.insert(g);
                }
            });
            (*id, callees)
        })
        .collect();
    calls
        .keys()
        .map(|&id| {
            let mut seen = BTreeSet::new();
            let mut todo: Vec<FuncId> = calls[&id].iter().copied().collect();
            while let Some(g) = todo.pop() {
                if seen.insert(g) {
                    todo.extend(calls.get(&g).into_iter().flatten());
                }
            }
            (id, seen)
        })
        .collect()
}

/// Whether `tail` only returns `binds`.
pub(super) fn returns(tail: &Tail, binds: &[Var]) -> bool {
    match tail {
        Tail::Jump {
            target: Label::RET,
            args,
            ..
        } => args.len() == binds.len() && args.iter().zip(binds).all(|(a, b)| *a == Atom::Var(*b)),
        _ => false,
    }
}

pub(super) fn each_let(b: &Block, f: &mut impl FnMut(&Let)) {
    let mut todo = vec![b];
    while let Some(b) = todo.pop() {
        b.lets.iter().for_each(&mut *f);
        todo.extend(b.children());
    }
}
