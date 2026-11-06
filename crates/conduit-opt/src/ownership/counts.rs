//! The `dup`s and `drop`s that make ownership explicit. See
//! [`insert_counts`].

use super::*;

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

pub(super) struct Counts<'b> {
    /// Whether each variable holds an object that's counted.
    pub(super) counted: Vec<bool>,
    /// The borrowed parameters.
    pub(super) lent: Set,
    pub(super) borrows: &'b Borrows,
    /// What the body of each join point uses from outside: what a jump to
    /// it hands over besides its arguments.
    pub(super) free: BTreeMap<Label, Set>,
}

impl Counts<'_> {
    pub(super) fn is(&self, v: Var) -> bool {
        self.counted.get(v.0 as usize).copied().unwrap_or(false)
    }

    pub(super) fn var(&self, a: &Atom) -> Option<Var> {
        match a {
            Atom::Var(v) if self.is(*v) => Some(*v),
            _ => None,
        }
    }

    /// Finds what each join point's body uses from outside: a fixpoint,
    /// since a body can jump to itself, or to another join that uses more.
    pub(super) fn find_free(&mut self, body: &Block) {
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
    pub(super) fn live(&self, b: &Block) -> Set {
        let mut live = self.live_tail(&b.tail);
        for l in b.lets.iter().rev() {
            self.step_back(l, &mut live);
        }
        live
    }

    /// `live` after `l` becomes `live` before it.
    pub(super) fn step_back(&self, l: &Let, live: &mut Set) {
        for v in &l.binds {
            live.remove(v);
        }
        live.extend(l.args.iter().filter_map(|a| self.var(a)));
    }

    pub(super) fn live_tail(&self, tail: &Tail) -> Set {
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
    pub(super) fn block(&self, b: Block, owned: &Set) -> Block {
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
    pub(super) fn let_(&self, l: Let, after: &Set, out: &mut Vec<Let>) {
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
pub(super) fn takes_with(b: &Borrows, op: &Op, i: usize) -> bool {
    match op {
        Op::Call(f, ..) => !b.is_borrowed(*f, i),
        _ => takes(op, i),
    }
}

/// Whether `op` takes the reference of its argument `i`, rather than
/// borrowing it, if every parameter of a call is owned.
pub(super) fn takes(op: &Op, i: usize) -> bool {
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
pub(super) fn dups(v: Var, n: u32, out: &mut Vec<Let>) {
    out.extend((1..n).map(|_| count(Op::Dup, v)));
}

pub(super) fn count(op: Op, v: Var) -> Let {
    Let {
        binds: Vec::new(),
        op,
        args: vec![Atom::Var(v)],
        source: None,
    }
}
