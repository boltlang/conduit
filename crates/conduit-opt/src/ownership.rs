//! Reference counts, made explicit: [`insert_counts`] adds the `dup`s and
//! `drop`s that "Reference counts" in `docs/ir.md` asks for, so that a
//! program frees every object once nothing refers to it any more, except in
//! cycles.
//!
//! It runs once, after optimization: the other passes don't keep counts
//! right. Every variable that holds an object owns one reference to it:
//!
//! - **Parameters** are owned, of functions and of join points, except the
//!   function parameters that [`borrows`] finds are borrowed: the caller
//!   keeps the reference, so the function never drops it, and `dup`s it
//!   for each use that takes one.
//! - **What an op gives** is owned, except what `get`, `array_get`,
//!   `ref_get` and `unbox` read out of an object, which is borrowed from
//!   it: a `dup` right after makes it owned, as Perceus does. Specializing
//!   `drop`s later cancels most of those against the `drop` of the object.
//! - **A use that takes a reference** (a field of `new`, a call's argument,
//!   a jump's) needs a `dup` before it if the variable is still used
//!   afterwards, or appears again among what the op takes. Its last use
//!   gives the reference away.
//! - **After the last use** that doesn't take the reference, the variable
//!   is dropped, and so is one whose value is never used. A branch, an arm
//!   of a `case` or the body of a `join`, drops what's owned when it starts
//!   but it doesn't use.
//!
//! A join point is like a local function, and the variables its body uses
//! from outside are owned by it: each jump to it hands them over with its
//! arguments, so they stay alive at every jump.
//!
//! Variables bound by `const` or `undefined` are never counted, since
//! their counts would do nothing.
//!
//! [`insert_reuse`] runs first: where an arm of a `case` takes an object
//! apart and builds one of the same shape, the new one goes in the old
//! one's memory when that's unique, instead of freeing one and allocating
//! the other: Perceus's "functional but in-place".
//!
//! [`specialize_drops`] then makes the counts cheaper where a field read
//! out of an object is kept and the object dropped: when the object turns
//! out to be unique at run time, its reference to the field becomes the
//! variable's, so neither is counted.

use std::collections::{BTreeMap, BTreeSet};

use conduit_ir::verify::resolve;
use conduit_ir::{
    Arm, Atom, Block, CtorId, DataDecl, DataId, Func, FuncId, JoinDef, Label, Let, Module,
    Mutability, Op, Tag, Tail, Ty, Var,
};

type Set = BTreeSet<Var>;

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
/// - A function that's made into a closure, or is a handler's clause, keeps
///   every parameter owned: `apply` and `perform` don't know whom they
///   call, so they always give their arguments away.
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
        always_owned.extend(h.ops.iter().copied());
        always_owned.insert(h.ret);
    }
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
fn lent_vars(id: FuncId, f: &Func, b: &Borrows) -> Set {
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
fn reaches(m: &Module) -> BTreeMap<FuncId, BTreeSet<FuncId>> {
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
fn returns(tail: &Tail, binds: &[Var]) -> bool {
    match tail {
        Tail::Jump {
            target: Label::RET,
            args,
            ..
        } => args.len() == binds.len() && args.iter().zip(binds).all(|(a, b)| *a == Atom::Var(*b)),
        _ => false,
    }
}

fn each_let(b: &Block, f: &mut impl FnMut(&Let)) {
    let mut todo = vec![b];
    while let Some(b) = todo.pop() {
        b.lets.iter().for_each(&mut *f);
        todo.extend(b.children());
    }
}

/// Adds `dup`s and `drop`s to every function of `m`, with the borrowed
/// parameters [`borrows`] infers. Fails if a function has counts already,
/// since they would be counted twice.
pub fn insert_counts(m: &mut Module) -> Result<(), String> {
    let b = borrows(m);
    insert_counts_with(m, &b)
}

/// [`insert_counts`] with the borrowed parameters given.
pub fn insert_counts_with(m: &mut Module, borrows: &Borrows) -> Result<(), String> {
    for (id, f) in &mut m.funcs {
        count_func(*id, f, borrows).map_err(|e| format!("{id}: {e}"))?;
    }
    Ok(())
}

/// Adds `dup`s and `drop`s to one function, `id`.
pub fn count_func(id: FuncId, f: &mut Func, borrows: &Borrows) -> Result<(), String> {
    let mut counted: Vec<bool> = f.vars.iter().map(Ty::is_object).collect();
    // A borrowed variable isn't the function's to count: it only gets a
    // `dup` for each use that takes a reference.
    let lent = lent_vars(id, f, borrows);
    for v in &lent {
        counted[v.0 as usize] = false;
    }
    let mut todo = vec![&f.body];
    while let Some(b) = todo.pop() {
        for l in &b.lets {
            match l.op {
                // `drop_reuse` comes before, from `insert_reuse`.
                ref op if op.is_count() && *op != Op::DropReuse => {
                    return Err("it has counts already".into());
                }
                Op::Const(_) | Op::Undefined => {
                    for v in &l.binds {
                        counted[v.0 as usize] = false;
                    }
                }
                _ => {}
            }
        }
        todo.extend(b.children());
    }
    let mut cx = Counts {
        counted,
        lent,
        borrows,
        free: BTreeMap::new(),
    };
    cx.find_free(&f.body);
    let owned: Set = f.params.iter().copied().filter(|v| cx.is(*v)).collect();
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    f.body = cx.block(body, &owned);
    Ok(())
}

struct Counts<'b> {
    /// Whether each variable holds an object that's counted.
    counted: Vec<bool>,
    /// The borrowed parameters.
    lent: Set,
    borrows: &'b Borrows,
    /// What the body of each join point uses from outside: what a jump to
    /// it hands over besides its arguments.
    free: BTreeMap<Label, Set>,
}

impl Counts<'_> {
    fn is(&self, v: Var) -> bool {
        self.counted.get(v.0 as usize).copied().unwrap_or(false)
    }

    fn var(&self, a: &Atom) -> Option<Var> {
        match a {
            Atom::Var(v) if self.is(*v) => Some(*v),
            _ => None,
        }
    }

    /// Finds what each join point's body uses from outside: a fixpoint,
    /// since a body can jump to itself, or to another join that uses more.
    fn find_free(&mut self, body: &Block) {
        let mut defs = Vec::new();
        let mut todo = vec![body];
        while let Some(b) = todo.pop() {
            if let Tail::Join { defs: ds, .. } = &b.tail {
                defs.extend(ds);
            }
            todo.extend(b.children());
        }
        loop {
            let mut changed = false;
            for def in &defs {
                let mut free = self.live(&def.body);
                for p in &def.params {
                    free.remove(p);
                }
                let old = self.free.entry(def.label).or_default();
                if *old != free {
                    *old = free;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// The counted variables live when `b` starts.
    fn live(&self, b: &Block) -> Set {
        let mut live = self.live_tail(&b.tail);
        for l in b.lets.iter().rev() {
            self.step_back(l, &mut live);
        }
        live
    }

    /// `live` after `l` becomes `live` before it.
    fn step_back(&self, l: &Let, live: &mut Set) {
        for v in &l.binds {
            live.remove(v);
        }
        live.extend(l.args.iter().filter_map(|a| self.var(a)));
    }

    fn live_tail(&self, tail: &Tail) -> Set {
        match tail {
            Tail::Jump { target, args, .. } => {
                let mut live: Set = args.iter().filter_map(|a| self.var(a)).collect();
                if let Some(free) = self.free.get(target) {
                    live.extend(free);
                }
                live
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                let mut live: Set = self.var(scrut).into_iter().collect();
                for arm in arms {
                    live.extend(self.live(&arm.body));
                }
                if let Some(d) = default {
                    live.extend(self.live(d));
                }
                live
            }
            Tail::Join { body, .. } => self.live(body),
            Tail::Unreachable | Tail::Trap => Set::new(),
        }
    }

    /// `b` with its counts, given what's owned when it starts.
    fn block(&self, b: Block, owned: &Set) -> Block {
        let live_in = self.live(&b);
        let mut lets = Vec::new();
        // A block that only stops needs nothing dropped first.
        let stops = b.lets.is_empty() && matches!(b.tail, Tail::Unreachable | Tail::Trap);
        if !stops {
            lets.extend(owned.difference(&live_in).map(|v| count(Op::Drop, *v)));
        }
        let tail_live = self.live_tail(&b.tail);
        let mut afters = vec![Set::new(); b.lets.len()];
        let mut live = tail_live.clone();
        for (i, l) in b.lets.iter().enumerate().rev() {
            afters[i] = live.clone();
            self.step_back(l, &mut live);
        }
        for (l, after) in b.lets.into_iter().zip(afters) {
            self.let_(l, &after, &mut lets);
        }
        let tail = match b.tail {
            Tail::Jump {
                target,
                args,
                source,
            } => {
                let keeps = self.free.get(&target);
                let mut taken: BTreeMap<Var, u32> = BTreeMap::new();
                for v in args.iter().filter_map(|a| self.var(a)) {
                    *taken.entry(v).or_default() += 1;
                }
                for (v, n) in taken {
                    let keep = keeps.is_some_and(|k| k.contains(&v)) as u32;
                    dups(v, n + keep, &mut lets);
                }
                for a in &args {
                    if let Atom::Var(v) = a
                        && self.lent.contains(v)
                    {
                        lets.push(count(Op::Dup, *v));
                    }
                }
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
            } => Tail::Case {
                scrut,
                arms: arms
                    .into_iter()
                    .map(|mut arm| {
                        arm.body = self.block(arm.body, &tail_live);
                        arm
                    })
                    .collect(),
                default: default.map(|d| Box::new(self.block(*d, &tail_live))),
            },
            Tail::Join { rec, defs, body } => Tail::Join {
                rec,
                defs: defs
                    .into_iter()
                    .map(|mut def| {
                        let mut owned: Set =
                            def.params.iter().copied().filter(|v| self.is(*v)).collect();
                        if let Some(free) = self.free.get(&def.label) {
                            owned.extend(free);
                        }
                        def.body = self.block(def.body, &owned);
                        def
                    })
                    .collect(),
                body: Box::new(self.block(*body, &tail_live)),
            },
            tail @ (Tail::Unreachable | Tail::Trap) => tail,
        };
        Block { lets, tail }
    }

    /// `l` with its counts, given what's live after it, into `out`.
    fn let_(&self, l: Let, after: &Set, out: &mut Vec<Let>) {
        let mut taken: BTreeMap<Var, u32> = BTreeMap::new();
        let mut borrowed = Set::new();
        for (i, a) in l.args.iter().enumerate() {
            let takes = takes_with(self.borrows, &l.op, i);
            if let Some(v) = self.var(a) {
                if takes {
                    *taken.entry(v).or_default() += 1;
                } else {
                    borrowed.insert(v);
                }
            } else if let Atom::Var(v) = a
                && takes
                && self.lent.contains(v)
            {
                out.push(count(Op::Dup, *v));
            }
        }
        // References each variable has to give, beyond the one it owns:
        // one to each use that takes one, and one to keep if it's live.
        for (&v, &n) in &taken {
            dups(v, n + after.contains(&v) as u32, out);
        }
        let dead: Vec<Var> = borrowed
            .iter()
            .filter(|v| !taken.contains_key(v) && !after.contains(v))
            .copied()
            .collect();
        let gives_borrowed = matches!(l.op, Op::Get(..) | Op::ArrayGet | Op::RefGet | Op::Unbox(_));
        let binds: Vec<Var> = l.binds.iter().copied().filter(|v| self.is(*v)).collect();
        out.push(l);
        // What was read out of an object first, before the object may be
        // dropped.
        if gives_borrowed {
            out.extend(
                binds
                    .iter()
                    .filter(|v| after.contains(v))
                    .map(|v| count(Op::Dup, *v)),
            );
        }
        out.extend(dead.into_iter().map(|v| count(Op::Drop, v)));
        if !gives_borrowed {
            out.extend(
                binds
                    .iter()
                    .filter(|v| !after.contains(v))
                    .map(|v| count(Op::Drop, *v)),
            );
        }
    }
}

/// Whether `op` takes the reference of its argument `i`, rather than
/// borrowing it, with the borrowed parameters of calls in `b`.
fn takes_with(b: &Borrows, op: &Op, i: usize) -> bool {
    match op {
        Op::Call(f, ..) => !b.is_borrowed(*f, i),
        _ => takes(op, i),
    }
}

/// Whether `op` takes the reference of its argument `i`, rather than
/// borrowing it, if every parameter of a call is owned.
fn takes(op: &Op, i: usize) -> bool {
    match op {
        Op::New(_)
        | Op::ArrayNew
        | Op::Closure(..)
        | Op::Call(..)
        | Op::Apply
        | Op::Handle(..)
        | Op::Perform(..)
        | Op::Resume
        | Op::Discontinue(..)
        | Op::RefNew
        | Op::Box(_)
        | Op::ArrayFreeze
        | Op::DropReuse
        | Op::ReuseToken
        | Op::Reuse(_) => true,
        Op::Set(..) | Op::RefSet | Op::ArrayFill => i == 1,
        Op::ArraySet => i == 2,
        _ => false,
    }
}

/// `dup`s so that `v`, owning one reference, has `n` to give.
fn dups(v: Var, n: u32, out: &mut Vec<Let>) {
    out.extend((1..n).map(|_| count(Op::Dup, v)));
}

fn count(op: Op, v: Var) -> Let {
    Let {
        binds: Vec::new(),
        op,
        args: vec![Atom::Var(v)],
        source: None,
    }
}

/// Specializes the `drop`s of every function of `m` that [`insert_counts`]
/// counted: see [`specialize_func`].
pub fn specialize_drops(m: &mut Module) {
    let data = &m.data;
    for f in m.funcs.values_mut() {
        specialize_func(data, f);
    }
}

/// Specializes a `drop(x)` of an `imm` data object, in a block where `dup`s
/// of fields read from `x` come before it:
///
/// ```text
/// let %t = get C.i(%x)          let %t = get C.i(%x)
/// let = dup(%t)                 let %u: i32 = is_unique(%x)
/// ..                            join k() = <the rest>
/// let = drop(%x)          =>    in case %u {
/// <the rest>                      1 => <drop x's other object fields>
///                                      free(%x); jump k()
///                                 _ => dup(%t); decref(%x); jump k()
///                               }
/// ```
///
/// When `x` is unique, its reference to the field is `%t`'s, so the `dup`
/// and the field's `drop` cancel. A `drop_reuse(x)` is specialized the
/// same way, with the token a parameter of `k`: `reuse_token(x)` when `x`
/// is unique, and `undefined`, no memory, when it's shared. Only `imm` objects, whose fields can't
/// have changed since they were read, and only where a `dup` cancels: a
/// `drop` with nothing to cancel is as fast as its expansion.
pub fn specialize_func(data: &BTreeMap<DataId, DataDecl>, f: &mut Func) {
    let mut reads = BTreeMap::new();
    let mut todo = vec![&f.body];
    while let Some(b) = todo.pop() {
        for l in &b.lets {
            if let (Op::Get(c, i), [Atom::Var(x)], [y]) = (&l.op, &l.args[..], &l.binds[..])
                && matches!(f.var_ty(*x), Ty::Data(_, Mutability::Imm, ..))
            {
                reads.insert(*y, (*x, *c, *i));
            }
        }
        todo.extend(b.children());
    }
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    f.body = Specialize { data, f, reads }.block(body);
}

/// A `dup` of a field that can move into the branches of a specialized
/// `drop`.
struct Sink {
    field: u32,
    /// Its index among the block's `let`s.
    at: usize,
    var: Var,
}

struct Specialize<'a> {
    data: &'a BTreeMap<DataId, DataDecl>,
    f: &'a mut Func,
    /// What each variable was read from by `get`, of an `imm` object,
    /// whose fields don't change: the object, the constructor and the
    /// field. A variable is bound once, before its uses, so this holds
    /// wherever it's used.
    reads: BTreeMap<Var, (Var, CtorId, u32)>,
}

impl Specialize<'_> {
    fn block(&mut self, mut b: Block) -> Block {
        self.push_dups(&mut b);
        cancel_dups(&mut b.lets);
        for d in 0..b.lets.len() {
            if let Some(clone) = self.freezes(&b.lets, d) {
                let mut rest_lets = b.lets.split_off(clone + 1);
                rest_lets.remove(d - clone - 1);
                let l = b.lets.pop().unwrap();
                let rest = self.block(Block {
                    lets: rest_lets,
                    tail: b.tail,
                });
                let mut lets = b.lets;
                let tail = self.freeze(l, rest, &mut lets);
                return Block { lets, tail };
            }
            if let Some((c, sinks)) = self.sinks(&b.lets, d) {
                let rest_lets = b.lets.split_off(d + 1);
                let drop = b.lets.pop().unwrap();
                let rest = self.block(Block {
                    lets: rest_lets,
                    tail: b.tail,
                });
                let mut lets = b.lets;
                let moved: Vec<(u32, Var)> = sinks.iter().map(|s| (s.field, s.var)).collect();
                let mut at: Vec<usize> = sinks.iter().map(|s| s.at).collect();
                at.sort_unstable();
                for s in at.into_iter().rev() {
                    lets.remove(s);
                }
                let token = drop.binds.first().copied();
                let tail = self.expand(&drop.args[0], c, &moved, token, rest, &mut lets);
                return Block { lets, tail };
            }
        }
        b.tail = match b.tail {
            Tail::Case {
                scrut,
                arms,
                default,
            } => Tail::Case {
                scrut,
                arms: arms
                    .into_iter()
                    .map(|mut arm| {
                        arm.body = self.block(arm.body);
                        arm
                    })
                    .collect(),
                default: default.map(|d| Box::new(self.block(*d))),
            },
            Tail::Join { rec, defs, body } => Tail::Join {
                rec,
                defs: defs
                    .into_iter()
                    .map(|mut def| {
                        def.body = self.block(def.body);
                        def
                    })
                    .collect(),
                body: Box::new(self.block(*body)),
            },
            tail => tail,
        };
        b
    }

    /// Moves the `dup`s at the end of `b` of fields read from an object that
    /// a branch of its `case` drops into the start of every branch, so that
    /// the `drop` there can be specialized: the `dup` is then in its block.
    /// Only a `dup` that nothing after it counts or gives away, neither the
    /// field nor the object, since a `drop` of the object could free the
    /// field.
    fn push_dups(&self, b: &mut Block) {
        let Tail::Case { arms, default, .. } = &mut b.tail else {
            return;
        };
        let mut dropped = Set::new();
        for body in arms.iter().map(|a| &a.body).chain(default.as_deref()) {
            dropped_in(body, &mut dropped);
        }
        let mut moved = Vec::new();
        for s in (0..b.lets.len()).rev() {
            let (Op::Dup, [Atom::Var(y)]) = (&b.lets[s].op, &b.lets[s].args[..]) else {
                continue;
            };
            let Some(&(x, ..)) = self.reads.get(y) else {
                continue;
            };
            let touched = b.lets[s + 1..].iter().any(|l| {
                (l.args.iter().enumerate()).any(|(p, a)| {
                    (*a == Atom::Var(*y) || *a == Atom::Var(x))
                        && (l.op.is_count() || takes(&l.op, p))
                })
            });
            if dropped.contains(&x) && !touched {
                moved.push(b.lets.remove(s));
            }
        }
        if moved.is_empty() {
            return;
        }
        moved.reverse();
        for body in arms
            .iter_mut()
            .map(|a| &mut a.body)
            .chain(default.as_deref_mut())
        {
            body.lets.splice(0..0, moved.iter().cloned());
        }
    }

    /// If `lets[d]` is a `drop(a)` whose array was last used by an
    /// `array_clone` to `imm` before it, with nothing in between that uses
    /// `a`: that clone's index.
    fn freezes(&self, lets: &[Let], d: usize) -> Option<usize> {
        let (Op::Drop, [a @ Atom::Var(_)]) = (&lets[d].op, &lets[d].args[..]) else {
            return None;
        };
        let (c, l) = lets[..d]
            .iter()
            .enumerate()
            .rev()
            .find(|(_, l)| l.args.contains(a))?;
        let imm = match &l.binds[..] {
            [b] => matches!(self.f.var_ty(*b), Ty::Array(_, Mutability::Imm)),
            _ => false,
        };
        (l.op == Op::ArrayClone && imm).then_some(c)
    }

    /// `let b = array_clone(a)`, then `rest`, without the `drop(a)` after
    /// it, as the array itself when it's unique:
    ///
    /// ```text
    /// let %u: i32 = is_unique(%a)
    /// join k(%b) = <rest>
    /// in case %u {
    ///   1 => let %b1 = array_freeze(%a); jump k(%b1)
    ///   _ => let %b2 = array_clone(%a); decref(%a); jump k(%b2)
    /// }
    /// ```
    fn freeze(&mut self, clone: Let, rest: Block, lets: &mut Vec<Let>) -> Tail {
        let a = clone.args[0].clone();
        let b = clone.binds[0];
        let ty = self.f.var_ty(b).clone();
        let u = self.f.new_var(Ty::I32);
        lets.push(Let {
            binds: vec![u],
            op: Op::IsUnique,
            args: vec![a.clone()],
            source: None,
        });
        let k = self.f.new_label();
        let (b1, b2) = (self.f.new_var(ty.clone()), self.f.new_var(ty));
        let jump = |v: Var| Tail::Jump {
            target: k,
            args: vec![Atom::Var(v)],
            source: None,
        };
        let frozen = Block {
            lets: vec![Let {
                binds: vec![b1],
                op: Op::ArrayFreeze,
                args: vec![a.clone()],
                source: clone.source,
            }],
            tail: jump(b1),
        };
        let copied = Block {
            lets: vec![
                Let {
                    binds: vec![b2],
                    op: Op::ArrayClone,
                    args: vec![a.clone()],
                    source: clone.source,
                },
                count_atom(Op::DecRef, &a),
            ],
            tail: jump(b2),
        };
        Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: k,
                params: vec![b],
                body: rest,
            }],
            body: Box::new(Block::from(Tail::Case {
                scrut: Atom::Var(u),
                arms: vec![Arm {
                    tag: Tag::Int(1),
                    body: frozen,
                }],
                default: Some(Box::new(copied)),
            })),
        }
    }

    /// If `lets[d]` is a `drop` to specialize: the constructor its object
    /// is known to be built by, and the `dup`s that can move into its
    /// branches.
    fn sinks(&self, lets: &[Let], d: usize) -> Option<(CtorId, Vec<Sink>)> {
        let l = &lets[d];
        let (Op::Drop | Op::DropReuse, [Atom::Var(x)]) = (&l.op, &l.args[..]) else {
            return None;
        };
        if !matches!(self.f.var_ty(*x), Ty::Data(_, Mutability::Imm, ..)) {
            return None;
        }
        let mut ctor = None;
        let mut sinks: Vec<Sink> = Vec::new();
        for (s, l) in lets[..d].iter().enumerate() {
            let (Op::Dup, [Atom::Var(y)], []) = (&l.op, &l.args[..], &l.binds[..]) else {
                continue;
            };
            let Some(&(o, c, i)) = self.reads.get(y) else {
                continue;
            };
            if o != *x {
                continue;
            }
            let used_later = lets[s + 1..d].iter().any(|l| {
                (l.args.iter().enumerate())
                    .any(|(p, a)| *a == Atom::Var(*y) && (l.op.is_count() || takes(&l.op, p)))
            });
            if !used_later && !sinks.iter().any(|sink| sink.field == i) {
                ctor = Some(c);
                sinks.push(Sink {
                    field: i,
                    at: s,
                    var: *y,
                });
            }
        }
        ctor.map(|c| (c, sinks))
    }

    /// The tail that replaces the `drop` of `x`, built by `c`, whose fields
    /// in `moved` are held by those variables, or the `drop_reuse` of it
    /// that binds `token`. Adds the `is_unique` to `lets`.
    fn expand(
        &mut self,
        x: &Atom,
        c: CtorId,
        moved: &[(u32, Var)],
        token: Option<Var>,
        rest: Block,
        lets: &mut Vec<Let>,
    ) -> Tail {
        let Atom::Var(xv) = x else { unreachable!() };
        let Ty::Data(_, m, margs, rows) = self.f.var_ty(*xv).clone() else {
            unreachable!()
        };
        let u = self.f.new_var(Ty::I32);
        lets.push(Let {
            binds: vec![u],
            op: Op::IsUnique,
            args: vec![x.clone()],
            source: None,
        });
        let k = self.f.new_label();
        let jump = |token: Option<Var>| Tail::Jump {
            target: k,
            args: token.map(Atom::Var).into_iter().collect(),
            source: None,
        };
        // For a `drop_reuse`, a token from each branch for the rest.
        let tokens = token.map(|t| {
            let ty = self.f.var_ty(t).clone();
            (self.f.new_var(ty.clone()), self.f.new_var(ty))
        });
        let fields = self.data[&c.data].ctors[c.index as usize].fields.clone();
        let mut unique = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            let ty = resolve(field, m, &margs, &rows);
            if !ty.is_object() || moved.iter().any(|&(j, _)| j as usize == i) {
                continue;
            }
            let v = self.f.new_var(ty);
            unique.push(Let {
                binds: vec![v],
                op: Op::Get(c, i as u32),
                args: vec![x.clone()],
                source: None,
            });
            unique.push(count(Op::Drop, v));
        }
        let mut shared: Vec<Let> = moved.iter().map(|&(_, y)| count(Op::Dup, y)).collect();
        shared.push(count_atom(Op::DecRef, x));
        match tokens {
            // Unique: its memory is the token. Shared: there's none.
            Some((t1, t2)) => {
                unique.push(Let {
                    binds: vec![t1],
                    op: Op::ReuseToken,
                    args: vec![x.clone()],
                    source: None,
                });
                shared.push(Let {
                    binds: vec![t2],
                    op: Op::Undefined,
                    args: Vec::new(),
                    source: None,
                });
            }
            None => unique.push(count_atom(Op::Free, x)),
        }
        let (t1, t2) = tokens.unzip();
        Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: k,
                params: token.into_iter().collect(),
                body: rest,
            }],
            body: Box::new(Block::from(Tail::Case {
                scrut: Atom::Var(u),
                arms: vec![Arm {
                    tag: Tag::Int(1),
                    body: Block {
                        lets: unique,
                        tail: jump(t1),
                    },
                }],
                default: Some(Box::new(Block {
                    lets: shared,
                    tail: jump(t2),
                })),
            })),
        }
    }
}

fn count_atom(op: Op, a: &Atom) -> Let {
    Let {
        binds: Vec::new(),
        op,
        args: vec![a.clone()],
        source: None,
    }
}

/// Adds reuse to every function of `m`, before [`insert_counts`]: in an arm
/// `C =>` of a `case` on `x` whose last use of `x` is in the arm's own
/// block, right after it, and before the first `new C'(..)` after it of a
/// constructor of the same shape (see
/// [`same_shape`](conduit_ir::verify::same_shape)), or, if there's none in
/// the block, at the start of each branch after it that has one:
///
/// ```text
/// let %t: reuse C = drop_reuse(%x)
/// ..
/// let %y = reuse C'(%t, ..)
/// ```
///
/// That builds `y` in `x`'s memory if `x` was unique, and allocates
/// otherwise. The `drop_reuse` is right after the last use, rather than at
/// the `new`, so that `x` doesn't keep its fields alive in between: a
/// recursive call on a field, as in `map`, then finds it unique. Only in
/// the arm's own block, so that the token is always used. `x` is then
/// owned where it was borrowed, which [`borrows`] sees.
pub fn insert_reuse(m: &mut Module) {
    let shapes = Module {
        data: m.data.clone(),
        ..Module::default()
    };
    for f in m.funcs.values_mut() {
        reuse_func(&shapes, f);
    }
}

fn reuse_func(shapes: &Module, f: &mut Func) {
    let none = Borrows::default();
    let mut cx = Counts {
        counted: f.vars.iter().map(Ty::is_object).collect(),
        lent: Set::new(),
        borrows: &none,
        free: BTreeMap::new(),
    };
    cx.find_free(&f.body);
    let mut todo = vec![&mut f.body];
    let vars = &mut f.vars;
    while let Some(b) = todo.pop() {
        match &mut b.tail {
            Tail::Case { scrut, arms, .. } => {
                for arm in arms.iter_mut() {
                    if let (Atom::Var(x), Tag::Ctor(c)) = (&*scrut, arm.tag)
                        && matches!(vars[x.0 as usize], Ty::Data(..))
                    {
                        reuse_arm(&cx, shapes, &mut arm.body, *x, c, vars);
                    }
                }
            }
            Tail::Join { .. } | Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
        }
        todo.extend(b.children_mut());
    }
}

/// Reuses `x`, built by `c`, for the first `new` of the same shape in `b`
/// after `x`'s last use, if there's one.
fn reuse_arm(
    cx: &Counts<'_>,
    shapes: &Module,
    b: &mut Block,
    x: Var,
    c: CtorId,
    vars: &mut Vec<Ty>,
) {
    let mut afters = vec![Set::new(); b.lets.len()];
    let mut live = cx.live_tail(&b.tail);
    for (i, l) in b.lets.iter().enumerate().rev() {
        afters[i] = live.clone();
        cx.step_back(l, &mut live);
    }
    // Where `x` is dead from: after its last use, if it's in the block.
    // A last use that gives `x` away leaves no memory to reuse. If `x` is
    // still live then, the tail uses it, and its branches can still reuse
    // it where they don't.
    let gives =
        |l: &Let| (l.args.iter().enumerate()).any(|(p, a)| *a == Atom::Var(x) && takes(&l.op, p));
    let from = match b.lets.iter().rposition(|l| l.args.contains(&Atom::Var(x))) {
        Some(i) if gives(&b.lets[i]) => return,
        Some(i) if !afters[i].contains(&x) => Some(i + 1),
        None if !live.contains(&x) => Some(0),
        _ => None,
    };
    let found = from.and_then(|from| {
        let at = b.lets[from..].iter().position(|l| match l.op {
            Op::New(c2) => conduit_ir::verify::same_shape(shapes, c, c2),
            _ => false,
        });
        at.map(|j| (from, from + j))
    });
    let Some((from, j)) = found else {
        // None here, so a branch after this block's last use of `x` that
        // builds one can reuse it, at its start, if it doesn't use `x`
        // itself: each arm of a `case`, and the body of a `join`, but not a
        // definition, which a loop can run more than once.
        match &mut b.tail {
            Tail::Case { arms, default, .. } => {
                for arm in arms {
                    reuse_arm(cx, shapes, &mut arm.body, x, c, vars);
                }
                if let Some(d) = default {
                    reuse_arm(cx, shapes, d, x, c, vars);
                }
            }
            Tail::Join { body, .. } => reuse_arm(cx, shapes, body, x, c, vars),
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
        }
        return;
    };
    vars.push(Ty::Reuse(c));
    let t = Var(vars.len() as u32 - 1);
    let Op::New(c2) = b.lets[j].op else {
        unreachable!()
    };
    b.lets[j].op = Op::Reuse(c2);
    b.lets[j].args.insert(0, Atom::Var(t));
    b.lets.insert(
        from,
        Let {
            binds: vec![t],
            op: Op::DropReuse,
            args: vec![Atom::Var(x)],
            source: None,
        },
    );
}

/// The variables a `drop` or `drop_reuse` in `b` takes, in its own block or
/// in the branches of its `case`s.
fn dropped_in(b: &Block, out: &mut Set) {
    for l in &b.lets {
        if let (Op::Drop | Op::DropReuse, [Atom::Var(x)]) = (&l.op, &l.args[..]) {
            out.insert(*x);
        }
    }
    if let Tail::Case { arms, default, .. } = &b.tail {
        for body in arms.iter().map(|a| &a.body).chain(default.as_deref()) {
            dropped_in(body, out);
        }
    }
}

/// Removes each `dup(y)` that a `drop(y)` follows in `lets` with no use of
/// `y` in between: together, they change nothing.
fn cancel_dups(lets: &mut Vec<Let>) {
    let mut s = 0;
    while s < lets.len() {
        if let (Op::Dup, [y]) = (&lets[s].op, &lets[s].args[..]) {
            let next = (s + 1..lets.len()).find(|&d| lets[d].args.contains(y));
            if let Some(d) = next
                && lets[d].op == Op::Drop
                && lets[d].args.len() == 1
            {
                lets.remove(d);
                lets.remove(s);
                continue;
            }
        }
        s += 1;
    }
}
