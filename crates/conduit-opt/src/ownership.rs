//! Reference counts, made explicit: [`insert_counts`] adds the `dup`s and
//! `drop`s that "Reference counts" in `docs/ir.md` asks for, so that a
//! program frees every object once nothing refers to it any more, except in
//! cycles.
//!
//! It runs once, after optimization: the other passes don't keep counts
//! right. Every variable that holds an object owns one reference to it:
//!
//! - **Parameters** are owned, of functions and of join points. (Borrowed
//!   parameters can come later, as a convention between a function and its
//!   callers.)
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
//! [`specialize_drops`] then makes the counts cheaper where a field read
//! out of an object is kept and the object dropped: when the object turns
//! out to be unique at run time, its reference to the field becomes the
//! variable's, so neither is counted.

use std::collections::{BTreeMap, BTreeSet};

use conduit_ir::verify::resolve;
use conduit_ir::{
    Arm, Atom, Block, CtorId, DataDecl, DataId, Func, JoinDef, Label, Let, Module, Mutability, Op,
    Tag, Tail, Ty, Var,
};

type Set = BTreeSet<Var>;

/// Adds `dup`s and `drop`s to every function of `m`. Fails if a function
/// has some already, since they would be counted twice.
pub fn insert_counts(m: &mut Module) -> Result<(), String> {
    for (id, f) in &mut m.funcs {
        count_func(f).map_err(|e| format!("{id}: {e}"))?;
    }
    Ok(())
}

/// Adds `dup`s and `drop`s to one function.
pub fn count_func(f: &mut Func) -> Result<(), String> {
    let mut counted: Vec<bool> = f.vars.iter().map(is_object).collect();
    let mut todo = vec![&f.body];
    while let Some(b) = todo.pop() {
        for l in &b.lets {
            match l.op {
                Op::Dup | Op::Drop | Op::IsUnique | Op::Free | Op::DecRef => {
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
        children(b, &mut todo);
    }
    let mut cx = Counts {
        counted,
        free: BTreeMap::new(),
    };
    cx.find_free(&f.body);
    let owned: Set = f.params.iter().copied().filter(|v| cx.is(*v)).collect();
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    f.body = cx.block(body, &owned);
    Ok(())
}

struct Counts {
    /// Whether each variable holds an object that's counted.
    counted: Vec<bool>,
    /// What the body of each join point uses from outside: what a jump to
    /// it hands over besides its arguments.
    free: BTreeMap<Label, Set>,
}

impl Counts {
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
            children(b, &mut todo);
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
            if let Some(v) = self.var(a) {
                if takes(&l.op, i) {
                    *taken.entry(v).or_default() += 1;
                } else {
                    borrowed.insert(v);
                }
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
/// borrowing it.
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
        | Op::ArrayFreeze => true,
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

fn is_object(ty: &Ty) -> bool {
    !matches!(ty, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64)
}

/// The blocks `b` ends in.
fn children<'b>(b: &'b Block, out: &mut Vec<&'b Block>) {
    match &b.tail {
        Tail::Join { defs, body, .. } => {
            out.extend(defs.iter().map(|d| &d.body));
            out.push(body);
        }
        Tail::Case { arms, default, .. } => {
            out.extend(arms.iter().map(|a| &a.body));
            out.extend(default.as_deref());
        }
        Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
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
/// and the field's `drop` cancel. Only `imm` objects, whose fields can't
/// have changed since they were read, and only where a `dup` cancels: a
/// `drop` with nothing to cancel is as fast as its expansion.
pub fn specialize_func(data: &BTreeMap<DataId, DataDecl>, f: &mut Func) {
    let body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
    f.body = Specialize { data, f }.block(body);
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
}

impl Specialize<'_> {
    fn block(&mut self, mut b: Block) -> Block {
        for d in 0..b.lets.len() {
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
                let tail = self.expand(&drop.args[0], c, &moved, rest, &mut lets);
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

    /// If `lets[d]` is a `drop` to specialize: the constructor its object
    /// is known to be built by, and the `dup`s that can move into its
    /// branches.
    fn sinks(&self, lets: &[Let], d: usize) -> Option<(CtorId, Vec<Sink>)> {
        let l = &lets[d];
        let (Op::Drop, [Atom::Var(x)]) = (&l.op, &l.args[..]) else {
            return None;
        };
        if !matches!(self.f.var_ty(*x), Ty::Data(_, Mutability::Imm, ..)) {
            return None;
        }
        // What each variable was read from, in this block.
        let mut reads: BTreeMap<Var, (CtorId, u32)> = BTreeMap::new();
        let mut ctor = None;
        let mut sinks: Vec<Sink> = Vec::new();
        for (s, l) in lets[..d].iter().enumerate() {
            match (&l.op, &l.args[..], &l.binds[..]) {
                (Op::Get(c, i), [Atom::Var(o)], [y]) if o == x => {
                    reads.insert(*y, (*c, *i));
                }
                (Op::Dup, [Atom::Var(y)], []) => {
                    let Some(&(c, i)) = reads.get(y) else {
                        continue;
                    };
                    let used_later = lets[s + 1..d].iter().any(|l| {
                        l.args.iter().enumerate().any(|(p, a)| {
                            *a == Atom::Var(*y) && (is_count(&l.op) || takes(&l.op, p))
                        })
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
                _ => {}
            }
        }
        ctor.map(|c| (c, sinks))
    }

    /// The tail that replaces the `drop` of `x`, built by `c`, whose fields
    /// in `moved` are held by those variables. Adds the `is_unique` to
    /// `lets`.
    fn expand(
        &mut self,
        x: &Atom,
        c: CtorId,
        moved: &[(u32, Var)],
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
        let jump = || Tail::Jump {
            target: k,
            args: Vec::new(),
            source: None,
        };
        let fields = self.data[&c.data].ctors[c.index as usize].fields.clone();
        let mut unique = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            let ty = resolve(field, m, &margs, &rows);
            if !is_object(&ty) || moved.iter().any(|&(j, _)| j as usize == i) {
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
        unique.push(count_atom(Op::Free, x));
        let mut shared: Vec<Let> = moved.iter().map(|&(_, y)| count(Op::Dup, y)).collect();
        shared.push(count_atom(Op::DecRef, x));
        Tail::Join {
            rec: false,
            defs: vec![JoinDef {
                label: k,
                params: Vec::new(),
                body: rest,
            }],
            body: Box::new(Block::from(Tail::Case {
                scrut: Atom::Var(u),
                arms: vec![Arm {
                    tag: Tag::Int(1),
                    body: Block {
                        lets: unique,
                        tail: jump(),
                    },
                }],
                default: Some(Box::new(Block {
                    lets: shared,
                    tail: jump(),
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

fn is_count(op: &Op) -> bool {
    matches!(
        op,
        Op::Dup | Op::Drop | Op::IsUnique | Op::Free | Op::DecRef
    )
}
