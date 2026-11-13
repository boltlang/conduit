//! Code specialized to its handler: a `handle` of a handler whose clauses
//! are all tail-resumptive, whose body is a known closure, becomes a call of
//! a copy of the body's function, then of the return clause. The copy, and
//! a copy of each function it calls that can reach a handled `perform`,
//! take the handler's environment as extra parameters, and each `perform`
//! of a handled effect in them calls its clause, as a plain function,
//! directly:
//!
//! ```text
//! let c = closure f(caps..)              let xs = call f_H(caps.., env..)
//! let rs = handle H(env.., c)      =>    let rs = call H.ret(env.., xs..)
//! ```
//!
//! where in `f_H`, and in each `g_H` it reaches:
//!
//! ```text
//! let ys = perform E.i(args..)     =>    let ys = call clause_i(env.., args..)
//! let zs = call g(args..)          =>    let zs = call g_H(args.., env..)
//! ```
//!
//! So a `perform` in a callee finds its handler without a handler frame or
//! a lookup, which `direct` (#24) can only do for one in the body's own
//! function, once inlining has put it there, and `inline` can then inline
//! the clause. This is a pass over the whole module, since it adds
//! functions; `optimize_module` runs it once the passes on each function
//! are done, and those again on what it changed.
//!
//! A clause called directly must run under the handlers outside its own,
//! as with a captured continuation. It does: the `handle` is gone, and no
//! handler runs between it and a `perform` in the copies, since that would
//! be a `handle` whose row has a handled effect, which stops the pass.
//!
//! When it applies:
//!
//! - **Every clause is tail-resumptive without `discontinue`** (see
//!   [`conduit_ir::clause`]), and the handler has no effect parameters.
//!   An aborting clause would need the `handle` to unwind to: see #68.
//! - **The body is a known closure**, made by `closure f(caps..)` in the
//!   same function, whose rows don't name a handled effect.
//! - **Only `perform`s and calls reach a handled effect** in the code it
//!   copies: any other op whose row has one, like an `apply`, a `handle`
//!   or a `resume`, stops it, as does a call that names a handled effect
//!   in its rows, since the effect then comes in through a parameter.
//! - **Nothing in the module discontinues a handled effect**, which could
//!   inject a `perform` of it into a continuation that holds the `handle`.
//! - **The copies are small enough**: at most [`MAX_COPIED`] `let`s and
//!   tails per `handle`, beyond those already copied for the same handler.

use std::collections::{BTreeMap, BTreeSet};

use conduit_ir::clause::{discontinues, plain_call};
use conduit_ir::types::subst_all;
use conduit_ir::{
    Atom, EffectId, Func, FuncId, HandlerDecl, HandlerId, Let, Module, Mutability, Op, Row, Tail,
    Var,
};

use crate::MAX_CALLER_SIZE;

/// The most a `handle` may copy, in `let`s and tails.
pub const MAX_COPIED: usize = MAX_CALLER_SIZE;

/// Specializes code to its handler wherever it can in `m`. Returns
/// whether anything changed.
pub fn specialize(m: &mut Module) -> bool {
    let discontinued: BTreeSet<EffectId> = (m.funcs.values())
        .flat_map(|f| f.body.all_lets())
        .filter_map(|l| match l.op {
            Op::Discontinue(e, _) => Some(e),
            _ => None,
        })
        .collect();
    let mut cx = Cx {
        next: m.funcs.keys().next_back().map_or(0, |f| f.0 + 1),
        copies: BTreeMap::new(),
        plains: BTreeMap::new(),
        discontinued,
    };
    let ids: Vec<FuncId> = m.funcs.keys().copied().collect();
    let mut changed = false;
    for id in ids {
        changed |= cx.func(m, id);
    }
    changed
}

struct Cx {
    next: u64,
    /// The copy of each function for each handler.
    copies: BTreeMap<(FuncId, HandlerId), FuncId>,
    /// Each clause as a plain function, by handler and slot.
    plains: BTreeMap<(HandlerId, usize), FuncId>,
    /// The effects that some `discontinue` performs.
    discontinued: BTreeSet<EffectId>,
}

/// The closures a function makes, by the variable they're bound to: the
/// function, mutability arguments, rows and captures of each.
type Closures = BTreeMap<Var, (FuncId, Vec<Mutability>, Vec<Row>, Vec<Atom>)>;

/// A `handle` to specialize, in its function.
struct Site {
    h: HandlerId,
    /// The body's function, with its mutability arguments, rows and
    /// captures.
    body: FuncId,
    margs: Vec<Mutability>,
    rows: Vec<Row>,
    caps: Vec<Atom>,
}

/// Whether `row` may perform an effect that `decl` handles.
fn touches(decl: &HandlerDecl, row: &Row) -> bool {
    row.is_any() || row.effects().iter().any(|&e| decl.handles(e))
}

impl Cx {
    fn fresh(&mut self) -> FuncId {
        self.next += 1;
        FuncId(self.next - 1)
    }

    /// Specializes the `handle`s of function `id` of `m`. Returns whether
    /// it changed.
    fn func(&mut self, m: &mut Module, id: FuncId) -> bool {
        let mut f = m.funcs[&id].clone();
        let mut closures = Closures::new();
        for l in f.body.all_lets() {
            if let (Op::Closure(g, ms, rs), [c]) = (&l.op, &l.binds[..]) {
                closures.insert(*c, (*g, ms.clone(), rs.clone(), l.args.clone()));
            }
        }
        let mut body = std::mem::replace(&mut f.body, Tail::Unreachable.into());
        let mut changed = false;
        body.for_each_mut(|b| {
            if !b.lets.iter().any(|l| matches!(l.op, Op::Handle(..))) {
                return;
            }
            for l in std::mem::take(&mut b.lets) {
                match self.specialized(m, &closures, &mut f, &l) {
                    Some(calls) => {
                        b.lets.extend(calls);
                        changed = true;
                    }
                    None => b.lets.push(l),
                }
            }
        });
        f.body = body;
        if changed {
            m.funcs.insert(id, f);
        }
        changed
    }

    /// What replaces `l`, in `f`, if it's a `handle` that can be
    /// specialized: a call of the copy of its body, then of the return
    /// clause.
    fn specialized(
        &mut self,
        m: &mut Module,
        closures: &Closures,
        f: &mut Func,
        l: &Let,
    ) -> Option<[Let; 2]> {
        let site = self.site(m, closures, l)?;
        let copy = match self.copies.get(&(site.body, site.h)) {
            Some(&copy) => copy,
            None => {
                let order = self.reach(m, site.h, site.body)?;
                self.make_copies(m, site.h, &order)
            }
        };
        let results = &m.funcs[&site.body].results;
        let xs: Vec<Var> = (results.iter())
            .map(|t| f.new_var(subst_all(t, &site.margs, &site.rows)))
            .collect();
        let env: Vec<Atom> = l.args[..l.args.len() - 1].to_vec();
        let mut args = site.caps;
        args.extend(env.iter().cloned());
        let call = Let {
            binds: xs.clone(),
            op: Op::Call(copy, site.margs, site.rows),
            args,
            source: l.source,
        };
        let mut ret_args = env;
        ret_args.extend(xs.into_iter().map(Atom::Var));
        let ret = Let {
            binds: l.binds.clone(),
            op: Op::Call(m.handlers[&site.h].ret, Vec::new(), Vec::new()),
            args: ret_args,
            source: l.source,
        };
        Some([call, ret])
    }

    /// The `handle` that `l` is, if it can be specialized, but for what
    /// its body reaches.
    fn site(&self, m: &Module, closures: &Closures, l: &Let) -> Option<Site> {
        let Op::Handle(h, rows) = &l.op else {
            return None;
        };
        let decl = m.handlers.get(h)?;
        if !rows.is_empty() || decl.effect_params != 0 {
            return None;
        }
        if decl.effects().any(|e| self.discontinued.contains(&e)) {
            return None;
        }
        for (_, _, c) in decl.clauses() {
            let cf = m.funcs.get(&c)?;
            let plain = plain_call(cf)?;
            if discontinues(&plain) || cf.mut_params.count != 0 || cf.effect_params != 0 {
                return None;
            }
        }
        let Some(Atom::Var(c)) = l.args.last() else {
            return None;
        };
        let (g, margs, crows, caps) = closures.get(c)?.clone();
        let body = m.funcs.get(&g)?;
        if body.params.len() != caps.len() || crows.iter().any(|r| touches(decl, r)) {
            return None;
        }
        Some(Site {
            h: *h,
            body: g,
            margs,
            rows: crows,
            caps,
        })
    }

    /// The functions to copy for `h`, from `root`, which aren't copied for
    /// it yet, if the pass applies.
    fn reach(&self, m: &Module, h: HandlerId, root: FuncId) -> Option<Vec<FuncId>> {
        let decl = &m.handlers[&h];
        let mut seen = BTreeSet::new();
        let mut order = Vec::new();
        let mut stack = vec![root];
        let mut size = 0;
        while let Some(g) = stack.pop() {
            if !seen.insert(g) || self.copies.contains_key(&(g, h)) {
                continue;
            }
            let gf = m.funcs.get(&g)?;
            size += gf.body.size();
            if size > MAX_COPIED {
                return None;
            }
            order.push(g);
            for l in gf.body.all_lets() {
                match &l.op {
                    Op::Perform(e, i) if decl.handles(*e) => {
                        decl.clause(*e, *i)?;
                    }
                    Op::Call(c, _, rows) => {
                        if rows.iter().any(|r| touches(decl, r)) {
                            return None;
                        }
                        if touches(decl, &l.row(m, gf)) {
                            m.funcs.get(c)?;
                            stack.push(*c);
                        }
                    }
                    _ if touches(decl, &l.row(m, gf)) => return None,
                    _ => {}
                }
            }
        }
        Some(order)
    }

    /// The plain function of the clause in `slot` of `h`.
    fn plain(&mut self, m: &mut Module, h: HandlerId, slot: usize) -> FuncId {
        if let Some(&id) = self.plains.get(&(h, slot)) {
            return id;
        }
        let decl = &m.handlers[&h];
        let (_, _, c) = decl.clauses().nth(slot).expect("a clause in every slot");
        let cf = &m.funcs[&c];
        let mut plain = plain_call(cf).expect("a tail-resumptive clause");
        let name = cf
            .debug
            .name
            .as_deref()
            .map_or(c.to_string(), str::to_string);
        plain.debug.name = Some(format!("{name}_plain").into());
        let id = self.fresh();
        m.funcs.insert(id, plain);
        self.plains.insert((h, slot), id);
        id
    }

    /// Copies `order`, the functions [`reach`](Self::reach) found, for
    /// `h`. Gives the copy of the first.
    fn make_copies(&mut self, m: &mut Module, h: HandlerId, order: &[FuncId]) -> FuncId {
        for &g in order {
            let id = self.fresh();
            self.copies.insert((g, h), id);
        }
        let decl = m.handlers[&h].clone();
        let hname = decl
            .debug_name
            .as_deref()
            .map_or(h.to_string(), str::to_string);
        let mut slots = BTreeMap::new();
        for (slot, (e, i, _)) in decl.clauses().enumerate() {
            slots.insert((e, i), self.plain(m, h, slot));
        }
        for &g in order {
            let src = &m.funcs[&g];
            let mut f = src.clone();
            // Named as the return clause names them.
            let ret = &m.funcs[&decl.ret];
            let mut env = Vec::new();
            for (i, t) in decl.env.iter().enumerate() {
                let v = f.new_var(t.clone());
                let name = ret.params.get(i).and_then(|p| ret.debug.vars.get(p));
                if let Some(name) = name {
                    f.debug.vars.insert(v, name.clone());
                }
                env.push(Atom::Var(v));
            }
            f.params.extend(env.iter().map(|a| match a {
                Atom::Var(v) => *v,
                Atom::Lit(_) => unreachable!(),
            }));
            if !f.borrowed.is_empty() {
                f.borrowed.resize(f.params.len(), false);
            }
            let mut row = f.row.clone();
            for e in decl.effects() {
                row = row.without(e);
            }
            f.row = row.union(&decl.row);
            let name = src
                .debug
                .name
                .as_deref()
                .map_or(g.to_string(), str::to_string);
            f.debug.name = Some(format!("{name}_{hname}").into());
            let copies = &self.copies;
            let p = &*m;
            f.body.for_each_mut(|b| {
                for l in &mut b.lets {
                    let reaches = touches(&decl, &l.row(p, src));
                    match l.op.clone() {
                        Op::Perform(e, i) if decl.handles(e) => {
                            l.op = Op::Call(slots[&(e, i)], Vec::new(), Vec::new());
                            let mut args = env.clone();
                            args.append(&mut l.args);
                            l.args = args;
                        }
                        Op::Call(c, ms, rs) if reaches => {
                            l.op = Op::Call(copies[&(c, h)], ms, rs);
                            l.args.extend(env.iter().cloned());
                        }
                        _ => {}
                    }
                }
            });
            let id = self.copies[&(g, h)];
            m.funcs.insert(id, f);
        }
        self.copies[&(order[0], h)]
    }
}
