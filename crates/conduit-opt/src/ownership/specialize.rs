//! Cheaper counts where an object's fields are kept and the object
//! dropped. See [`specialize_drops`].

use super::*;

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
pub(super) struct Sink {
    field: u32,
    /// Its index among the block's `let`s.
    at: usize,
    var: Var,
}

pub(super) struct Specialize<'a> {
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

pub(super) fn count_atom(op: Op, a: &Atom) -> Let {
    Let {
        binds: Vec::new(),
        op,
        args: vec![a.clone()],
        source: None,
    }
}

/// The variables a `drop` or `drop_reuse` in `b` takes, in its own block or
/// in the branches of its `case`s.
pub(super) fn dropped_in(b: &Block, out: &mut Set) {
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
pub(super) fn cancel_dups(lets: &mut Vec<Let>) {
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
