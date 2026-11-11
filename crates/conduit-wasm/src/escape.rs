//! Which objects go on the shadow stack instead of the heap: those that
//! never outlive the function that makes them, so that the function's
//! frame, popped when it returns, can hold them.
//!
//! An object escapes if a reference to it can be kept after its function
//! returns: if it's returned, stored in an object, a ref, an array or a
//! closure, boxed, performed, handled, resumed or discontinued, passed to
//! an `apply`, or passed to a call that keeps its parameter, which this
//! finds for every function at once, as a fixpoint. A call in tail
//! position keeps all of its arguments, since the frame is popped before
//! it, and keeping it a tail call, so that a loop written as tail calls
//! runs in constant space, matters more. Join points pass objects on: an
//! object escapes if a parameter it's passed to does, and `array_freeze`
//! gives the same object again.
//!
//! An object made inside a recursive join point stays on the heap, since a
//! loop would push one for each time around it.
//!
//! What counting needs is in the header: a stack object is marked as
//! buffered, which keeps `release_obj` from giving its memory back and
//! `drop_reuse` from reusing it, while its count still drops what it holds
//! when it reaches 0 (see `counts.rs`).

use std::collections::{BTreeMap, HashSet};

use conduit_ir::{Atom, Block, Func, FuncId, Label, Let, Module, Op, Tail, Var};

/// The `let`s of `m`'s functions that make an object that goes on the
/// shadow stack, by their address.
pub(crate) fn stack_objects(m: &Module) -> HashSet<*const Let> {
    // Whether each function's parameters escape: none, at first, and then
    // more until nothing changes.
    let mut keeps: BTreeMap<FuncId, Vec<bool>> = (m.funcs.iter())
        .map(|(&id, f)| (id, vec![false; f.params.len()]))
        .collect();
    loop {
        let mut changed = false;
        for (&id, f) in &m.funcs {
            let flow = Flow::of(f, &keeps);
            let now: Vec<bool> = f.params.iter().map(|&p| flow.escapes(p)).collect();
            if keeps[&id] != now {
                keeps.insert(id, now);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = HashSet::new();
    for f in m.funcs.values() {
        let flow = Flow::of(f, &keeps);
        let mut todo = vec![(&f.body, false)];
        while let Some((b, in_loop)) = todo.pop() {
            if !in_loop {
                for l in &b.lets {
                    if let [v] = l.binds[..]
                        && allocates(&l.op)
                        && !flow.escapes(v)
                    {
                        out.insert(l as *const Let);
                    }
                }
            }
            match &b.tail {
                Tail::Join { rec, defs, body } => {
                    todo.extend(defs.iter().map(|d| (&d.body, in_loop || *rec)));
                    todo.push((body, in_loop));
                }
                Tail::Case { arms, default, .. } => {
                    todo.extend(arms.iter().map(|a| (&a.body, in_loop)));
                    todo.extend(default.as_deref().map(|d| (d, in_loop)));
                }
                Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
            }
        }
    }
    out
}

/// Whether `op` makes a new object, which can go on the stack.
fn allocates(op: &Op) -> bool {
    matches!(
        op,
        Op::New(_)
            | Op::ArrayNew
            | Op::ArrayFill
            | Op::ArrayAlloc
            | Op::ArrayClone
            | Op::RefNew
            | Op::Closure(..)
    )
}

/// Where a function's objects go: which variables may hold the same
/// object, through join points and `array_freeze`, as a union-find, and
/// which of those may escape.
struct Flow {
    parent: Vec<usize>,
    escapes: Vec<bool>,
}

impl Flow {
    fn of(f: &Func, keeps: &BTreeMap<FuncId, Vec<bool>>) -> Flow {
        let n = f.vars.len();
        let mut flow = Flow {
            parent: (0..n).collect(),
            escapes: vec![false; n],
        };
        let defs = conduit_ir::tail::join_defs(&f.body, f.labels);
        let mut escaping = Vec::new();
        for b in f.body.blocks() {
            for (i, l) in b.lets.iter().enumerate() {
                if l.op == Op::ArrayFreeze
                    && let ([to], [Atom::Var(from)]) = (&l.binds[..], &l.args[..])
                {
                    flow.union(*to, *from);
                }
                let tail = is_tail(b, i, &defs);
                for (p, a) in l.args.iter().enumerate() {
                    if let Atom::Var(v) = a
                        && (tail || kept(&l.op, p, keeps))
                    {
                        escaping.push(*v);
                    }
                }
            }
            if let Tail::Jump { target, args, .. } = &b.tail {
                let to = match defs.get(target.0 as usize) {
                    Some(Some(d)) => &d.params[..],
                    _ => &[],
                };
                for (i, a) in args.iter().enumerate() {
                    let Atom::Var(v) = a else { continue };
                    match to.get(i) {
                        Some(&p) if *target != Label::RET => flow.union(*v, p),
                        // Returned, or to a label that isn't there.
                        _ => escaping.push(*v),
                    }
                }
            }
        }
        for v in escaping {
            let r = flow.find(v.0 as usize);
            flow.escapes[r] = true;
        }
        flow
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: Var, b: Var) {
        let (a, b) = (self.find(a.0 as usize), self.find(b.0 as usize));
        if a != b {
            self.parent[a] = b;
            self.escapes[b] |= self.escapes[a];
        }
    }

    fn escapes(&self, v: Var) -> bool {
        let mut x = v.0 as usize;
        while self.parent[x] != x {
            x = self.parent[x];
        }
        self.escapes[x]
    }
}

/// Whether the `i`th `let` of `b` is in tail position, which the backend
/// lowers as a tail call when it's a call: the last, with `b` returning
/// just what it binds, as `conduit_ir::tail::returns` has it.
fn is_tail(b: &Block, i: usize, defs: &[Option<&conduit_ir::JoinDef>]) -> bool {
    i + 1 == b.lets.len() && conduit_ir::tail::returns(&b.lets[i].binds, &b.tail, defs)
}

/// Whether `op` may keep its `p`th argument after it's done: store it,
/// give it to code that may, or give it back as something else.
fn kept(op: &Op, p: usize, keeps: &BTreeMap<FuncId, Vec<bool>>) -> bool {
    match op {
        Op::Call(g, ..) => keeps
            .get(g)
            .is_none_or(|k| k.get(p).copied().unwrap_or(true)),
        // What's only read, counted, or copied from.
        Op::Prim(_)
        | Op::Get(..)
        | Op::ArrayGet
        | Op::ArrayLoad
        | Op::ArrayStore
        | Op::ArrayLen
        | Op::RefGet
        | Op::ArrayClone
        | Op::ArrayCopy
        | Op::Unbox(_)
        | Op::CallExtern(_)
        | Op::Dup
        | Op::Drop
        | Op::Free
        | Op::DecRef
        | Op::IsUnique
        | Op::DropReuse
        | Op::ReuseToken => false,
        // Into what's written, not what it's written into.
        Op::Set(..) | Op::RefSet => p == 1,
        Op::ArraySet => p == 2,
        Op::ArrayFill => p == 1,
        // The same object again, which the caller follows.
        Op::ArrayFreeze => false,
        // The closure's captures are read out, and then it's dropped.
        Op::Apply => p > 0,
        _ => true,
    }
}
