//! Evidence passing: removes effect handlers from a module, so that any
//! backend can run it. See "Evidence passing" in `docs/optimisations.md`.
//!
//! After [`lower_effects`], a module has no `handle`, `perform`, `resume`
//! or `discontinue`, no continuation types, and no rows: everything is
//! ordinary Conduit code, which verifies, runs in the evaluator and lowers
//! to Wasm without stack switching. It follows Koka's design (Xie and
//! Leijen, "Generalized evidence passing for effect handlers").
//!
//! - **Evidence.** A function that may perform, or that applies a
//!   closure, handles, or calls such a function, takes an evidence value
//!   as its last parameter: the active handlers, and a shared state that
//!   says whether the code is yielding to one. Every closure takes it too.
//!   Each such function keeps its ID as a wrapper with its old signature,
//!   which makes a fresh evidence value and calls the new version.
//! - **Handlers.** `handle` makes a node for its handler, with a fresh
//!   marker, a closure for each tail-resumptive clause, and the handlers
//!   outside, and calls the handler's `drive` function. That runs the
//!   body, then the return clause, or a clause the body yielded to, with a
//!   continuation, or passes an outer yield on.
//! - **`perform`** finds the nearest node of a handler of its effect. A tail-resumptive
//!   clause is called directly, under the handlers outside it. Any other
//!   clause is yielded to: `perform` records the operation, its arguments
//!   and the rest of its function, and returns undefined results.
//! - **Yielding.** After a call that may yield, the code checks the state,
//!   and if it's yielding, adds the rest of its function to the
//!   continuation being built, as a new function, and returns. Join points
//!   in scope there become functions first, since the rest can jump to
//!   them. Which calls may yield comes from rows: an effect yields if one
//!   of its handlers has a clause that isn't tail-resumptive, or performs
//!   one that does.
//! - **Continuations** are closures that take a closure giving what the
//!   `perform` gives: `resume` passes one giving the values, and
//!   `discontinue` one that performs its operation there. A flag in each
//!   makes a second use trap. Resuming one runs the handler's `drive`
//!   again, under the handlers around the `resume`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use conduit_ir::clause::{discontinues, plain_call};
use conduit_ir::{
    Arm, Atom, Block, ConstDecl, ConstId, ConstValue, CtorDecl, CtorId, DataDecl, DataId, EffectId,
    Func, FuncId, HandlerId, JoinDef, Label, Let, Lit, Module, Mutability, Op, Prim, Row, Sig, Tag,
    Tail, Ty, Var,
};

/// Removes effect handlers from `m`. See the module's documentation.
pub fn lower_effects(m: &mut Module) -> Result<(), String> {
    if m.handlers.is_empty() && !uses_effects(m) {
        return Ok(());
    }
    let mut cx = Lower::new(m)?;
    cx.prepare()?;
    cx.emit()?;
    *m = cx.out;
    Ok(())
}

/// Whether some function has a row, a continuation or an effect op.
fn uses_effects(m: &Module) -> bool {
    m.funcs.values().any(|f| {
        !f.row.is_nothing()
            || any_let(&f.body, &mut |l| {
                matches!(
                    l.op,
                    Op::Perform(..) | Op::Resume | Op::Discontinue(..) | Op::Handle(..)
                )
            })
    })
}

/// The run-time data types and helper functions the lowered code uses.
struct Runtime {
    /// `Yield`: the marker of the handler being yielded to (0 when not
    /// yielding), the operation, its arguments, the continuation built so
    /// far, and the next free marker.
    yield_d: DataId,
    /// `Ev`: the state, and the innermost handler node.
    ev_d: DataId,
    /// `Hs`: no handlers, or the innermost node.
    hs_d: DataId,
    /// `Node`: its handler's table of where each effect's clauses start
    /// among its own, by the effect's number, or -1 if it doesn't handle
    /// it; its marker, whether each clause is tail-resumptive, a closure
    /// for each that is, and the evidence outside it, which is set each
    /// time it runs.
    node_d: DataId,
    /// `lookup(ev, effect) -> (node, start)`, the nearest node that
    /// handles the effect and where the effect's clauses start among its
    /// own, trapping if there's none.
    lookup: FuncId,
    /// `compose(prev, next, x, ev)`: a continuation that runs `prev`, then
    /// `next`.
    compose: FuncId,
    /// `start_resume(cont, x, ev)`: applies a continuation, as a body.
    start_resume: FuncId,
}

impl Runtime {
    fn ev_ty(&self) -> Ty {
        Ty::Data(self.ev_d, Mutability::Mut, Vec::new(), Vec::new())
    }

    fn yield_ty(&self) -> Ty {
        Ty::Data(self.yield_d, Mutability::Mut, Vec::new(), Vec::new())
    }

    fn hs_ty(&self) -> Ty {
        Ty::Data(self.hs_d, Mutability::Mut, Vec::new(), Vec::new())
    }

    fn node_ty(&self) -> Ty {
        Ty::Data(self.node_d, Mutability::Mut, Vec::new(), Vec::new())
    }

    fn node(&self) -> CtorId {
        Runtime::ctor(self.node_d, 0)
    }

    /// A piece of a continuation: `fn(x, ev) -> any`, where `x` holds what
    /// the code it continues gave, and it gives what its function gives.
    fn piece_ty(&self) -> Ty {
        Ty::Fn(
            Box::new(Sig::new(vec![any(), self.ev_ty()], vec![any()])),
            Mutability::Mut,
        )
    }

    /// What `drive` runs: `fn(ev) -> any`.
    fn start_ty(&self) -> Ty {
        Ty::Fn(
            Box::new(Sig::new(vec![self.ev_ty()], vec![any()])),
            Mutability::Mut,
        )
    }

    fn ctor(d: DataId, index: u32) -> CtorId {
        CtorId { data: d, index }
    }
}

/// `ro any`, which holds any value.
fn any() -> Ty {
    Ty::Any(Mutability::ReadOnly)
}

/// The functions a handler is lowered to.
struct HandlerFuncs {
    /// Its nodes' table: for each effect's number, where its clauses
    /// start, or -1.
    starts: ConstId,
    /// For each clause, the plain clause if it's tail-resumptive.
    fast: Vec<Option<FuncId>>,
    /// `start_body(body, ev) -> any`.
    start_body: FuncId,
    /// `drive(ev, env.., node, start) -> R`.
    drive: FuncId,
    /// `reprompt(env.., node, inner, x, ev) -> any`.
    reprompt: FuncId,
    /// `kont(env.., node, cont, used, x, ev) -> R`: the continuation.
    kont: FuncId,
}

/// A call that may yield, and isn't a tail call: the code after it moves
/// to a function, the rest, which the continuation calls.
#[derive(Clone)]
struct Site {
    /// The variables the rest uses but doesn't bind, other than what the
    /// site binds: it's given them, after the evidence, in this order.
    free: Vec<Var>,
    /// What the site binds, which the rest is given in a box.
    binds: Vec<Var>,
    /// The function of the rest.
    rest: FuncId,
}

struct Lower {
    /// The module as given, with the functions the pass adds before
    /// lowering: lifted join points, plain clauses and the functions
    /// `discontinue` performs with.
    src: Module,
    out: Module,
    rt: Runtime,
    next_func: u64,
    next_data: u64,
    /// The effects that may yield.
    yields: BTreeSet<EffectId>,
    /// The functions that take evidence.
    takes_ev: BTreeSet<FuncId>,
    /// The new ID of each function that takes evidence, whose old ID is
    /// its wrapper.
    internal: BTreeMap<FuncId, FuncId>,
    /// For functions that don't take evidence, used in a closure: the
    /// function that takes it and calls them.
    ev_wrappers: BTreeMap<FuncId, FuncId>,
    handlers: BTreeMap<HandlerId, HandlerFuncs>,
    /// The function `discontinue E.i` performs with, by what the
    /// continuation takes, which it gives.
    perf: HashMap<(EffectId, u32, Vec<Ty>), FuncId>,
    /// `const(vals.., ev) -> vals`, by the types of the values.
    consts: HashMap<Vec<Ty>, FuncId>,
    /// The adapter of a `perform`'s continuation, by effect and operation.
    force: BTreeMap<(EffectId, u32), FuncId>,
    /// Tuples to box several values in, by their types.
    tuples: HashMap<(Vec<Ty>, u32), DataId>,
    /// The sites of each function, by the address of their `let`.
    sites: HashMap<*const Let, Site>,
}

// ---------------------------------------------------------------------
// Building IR.

fn new_func(params: Vec<Ty>, results: Vec<Ty>) -> Func {
    Func::new(params, results)
}

fn let1(f: &mut Func, lets: &mut Vec<Let>, ty: Ty, op: Op, args: Vec<Atom>) -> Var {
    let v = f.new_var(ty);
    lets.push(Let {
        binds: vec![v],
        op,
        args,
        source: None,
    });
    v
}

fn letn(f: &mut Func, lets: &mut Vec<Let>, tys: Vec<Ty>, op: Op, args: Vec<Atom>) -> Vec<Var> {
    let vs: Vec<Var> = tys.into_iter().map(|t| f.new_var(t)).collect();
    lets.push(Let {
        binds: vs.clone(),
        op,
        args,
        source: None,
    });
    vs
}

fn let0(lets: &mut Vec<Let>, op: Op, args: Vec<Atom>) {
    lets.push(Let {
        binds: Vec::new(),
        op,
        args,
        source: None,
    });
}

fn v(var: Var) -> Atom {
    Atom::Var(var)
}

fn int(n: i32) -> Atom {
    Atom::Lit(Lit::I32(n))
}

fn vs(vars: &[Var]) -> Vec<Atom> {
    vars.iter().map(|&x| v(x)).collect()
}

fn ret(args: Vec<Atom>) -> Tail {
    Tail::Jump {
        target: Label::RET,
        args,
        source: None,
    }
}

fn block(lets: Vec<Let>, tail: Tail) -> Block {
    Block { lets, tail }
}

/// `case x { 0 => zero, _ => other }`.
fn case_zero(x: Var, zero: Block, other: Block) -> Tail {
    Tail::Case {
        scrut: v(x),
        arms: vec![Arm {
            tag: Tag::Int(0),
            body: zero,
        }],
        default: Some(Box::new(other)),
    }
}

/// The mutability parameters of a function as arguments to another with
/// the same ones.
fn same_margs(f: &Func) -> Vec<Mutability> {
    (1..=f.mut_params.count).map(Mutability::Param).collect()
}

fn any_let(b: &Block, test: &mut impl FnMut(&Let) -> bool) -> bool {
    let mut found = false;
    b.walk(|b| found |= b.lets.iter().any(&mut *test));
    found
}

/// The variables `b` uses but doesn't bind, in order.
fn free_vars(b: &Block) -> BTreeSet<Var> {
    free_vars_of(&b.lets, &b.tail)
}

/// [`free_vars`] of the block that `lets` and `tail` would make, without
/// making it, which would copy what's nested in `tail`.
fn free_vars_of(lets: &[Let], tail: &Tail) -> BTreeSet<Var> {
    let mut used = BTreeSet::new();
    let mut bound = BTreeSet::new();
    let mut visit = |lets: &[Let], tail: &Tail| {
        for l in lets {
            for a in &l.args {
                if let Atom::Var(x) = a {
                    used.insert(*x);
                }
            }
            bound.extend(l.binds.iter().copied());
        }
        match tail {
            Tail::Jump { args, .. } => {
                for a in args {
                    if let Atom::Var(x) = a {
                        used.insert(*x);
                    }
                }
            }
            Tail::Case { scrut, .. } => {
                if let Atom::Var(x) = scrut {
                    used.insert(*x);
                }
            }
            Tail::Join { defs, .. } => {
                for d in defs {
                    bound.extend(d.params.iter().copied());
                }
            }
            Tail::Unreachable | Tail::Trap => {}
        }
    };
    visit(lets, tail);
    let children: Vec<&Block> = match tail {
        Tail::Join { defs, body, .. } => defs.iter().map(|d| &d.body).chain([&**body]).collect(),
        Tail::Case { arms, default, .. } => (arms.iter().map(|a| &a.body))
            .chain(default.as_deref())
            .collect(),
        Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => Vec::new(),
    };
    for c in children {
        c.walk(|b| visit(&b.lets, &b.tail));
    }
    used.difference(&bound).copied().collect()
}

/// The labels `b` jumps to.
fn jumped_labels(b: &Block) -> BTreeSet<Label> {
    let mut labels = BTreeSet::new();
    b.walk(|b| {
        if let Tail::Jump { target, .. } = &b.tail {
            labels.insert(*target);
        }
    });
    labels
}

/// The labels `b` defines.
fn defined_labels(b: &Block) -> BTreeSet<Label> {
    let mut labels = BTreeSet::new();
    b.walk(|b| {
        if let Tail::Join { defs, .. } = &b.tail {
            labels.extend(defs.iter().map(|d| d.label));
        }
    });
    labels
}

// ---------------------------------------------------------------------
// Setting up.

impl Lower {
    fn new(m: &Module) -> Result<Lower, String> {
        let next_func = m.funcs.keys().map(|f| f.0 + 1).max().unwrap_or(0);
        let next_data = m.data.keys().map(|d| d.0 + 1).max().unwrap_or(0);
        let mut cx = Lower {
            src: m.clone(),
            out: Module::default(),
            rt: Runtime {
                yield_d: DataId(next_data),
                ev_d: DataId(next_data + 1),
                hs_d: DataId(next_data + 2),
                node_d: DataId(next_data + 3),
                lookup: FuncId(next_func),
                compose: FuncId(next_func + 1),
                start_resume: FuncId(next_func + 2),
            },
            next_func: next_func + 3,
            next_data: next_data + 4,
            yields: BTreeSet::new(),
            takes_ev: BTreeSet::new(),
            internal: BTreeMap::new(),
            ev_wrappers: BTreeMap::new(),
            handlers: BTreeMap::new(),
            perf: HashMap::new(),
            consts: HashMap::new(),
            force: BTreeMap::new(),
            tuples: HashMap::new(),
            sites: HashMap::new(),
        };
        cx.runtime_data();
        Ok(cx)
    }

    fn fresh_func(&mut self) -> FuncId {
        self.next_func += 1;
        FuncId(self.next_func - 1)
    }

    fn fresh_data(&mut self) -> DataId {
        self.next_data += 1;
        DataId(self.next_data - 1)
    }

    fn runtime_data(&mut self) {
        let rt = &self.rt;
        let decl = |debug: &str, ctors: Vec<Vec<Ty>>| DataDecl {
            params: 0,
            effect_params: 0,
            ctors: ctors
                .into_iter()
                .map(|fields| CtorDecl { fields })
                .collect(),
            debug_name: Some(debug.into()),
        };
        let yield_decl = decl(
            "Yield",
            vec![vec![Ty::I32, Ty::I32, any(), rt.piece_ty(), Ty::I32]],
        );
        let ev_decl = decl("Ev", vec![vec![rt.yield_ty(), rt.hs_ty()]]);
        let hs_decl = decl("Hs", vec![vec![], vec![rt.node_ty()]]);
        let node_decl = decl(
            "Node",
            vec![vec![
                Ty::Array(Box::new(Ty::I32), Mutability::Imm),
                Ty::I32,
                Ty::Array(Box::new(Ty::I32), Mutability::Mut),
                Ty::Array(Box::new(any()), Mutability::Mut),
                rt.ev_ty(),
            ]],
        );
        self.out.data.insert(rt.yield_d, yield_decl);
        self.out.data.insert(rt.ev_d, ev_decl);
        self.out.data.insert(rt.hs_d, hs_decl);
        self.out.data.insert(rt.node_d, node_decl);
    }

    /// The number of an effect, which handler nodes hold.
    fn effect_number(&self, e: EffectId) -> i32 {
        self.src.effects.keys().position(|&k| k == e).unwrap_or(0) as i32 + 1
    }

    /// A constant for the nodes of `h`: for each effect's number, where
    /// its clauses start among those of `h`, or -1.
    fn starts_table(&mut self, h: HandlerId) -> ConstId {
        let decl = &self.src.handlers[&h];
        let mut starts = vec![-1; self.src.effects.len() + 1];
        let mut start = 0;
        for (e, clauses) in &decl.ops {
            starts[self.effect_number(*e) as usize] = start;
            start += clauses.len() as i32;
        }
        let next = (self.src.consts.keys().map(|c| c.0 + 1).max().unwrap_or(0))
            .max(self.out.consts.keys().map(|c| c.0 + 1).max().unwrap_or(0));
        let id = ConstId(next);
        let name = decl.debug_name.as_deref().unwrap_or("handler");
        self.out.consts.insert(
            id,
            ConstDecl {
                ty: Ty::Array(Box::new(Ty::I32), Mutability::Imm),
                value: ConstValue::Array(
                    starts
                        .into_iter()
                        .map(|s| ConstValue::Lit(Lit::I32(s)))
                        .collect(),
                ),
                debug_name: Some(format!("{name}_starts").into()),
            },
        );
        id
    }

    /// Whether code that performs `row` may yield.
    fn row_yields(&self, row: &Row) -> bool {
        row.is_any()
            || !row.params().is_empty()
            || row.effects().iter().any(|e| self.yields.contains(e))
    }

    /// Whether clause `i` of `h`, among all of them, is called directly, as
    /// a plain function.
    fn is_fast(&self, h: HandlerId, i: usize) -> Option<Func> {
        let (_, _, clause) = self.src.handlers[&h].clauses().nth(i)?;
        let plain = plain_call(self.src.funcs.get(&clause)?)?;
        (!discontinues(&plain)).then_some(plain)
    }

    fn prepare(&mut self) -> Result<(), String> {
        // Which effects may yield: those with a clause that isn't
        // tail-resumptive, and those whose clauses may perform one that
        // yields.
        loop {
            let mut changed = false;
            let ids: Vec<HandlerId> = self.src.handlers.keys().copied().collect();
            for h in ids {
                let decl = &self.src.handlers[&h];
                let row_yields = self.row_yields(&decl.row);
                let mut slot = 0;
                let mut found = Vec::new();
                for (e, clauses) in &decl.ops {
                    let slots = slot..slot + clauses.len();
                    slot += clauses.len();
                    if self.yields.contains(e) {
                        continue;
                    }
                    if row_yields || slots.into_iter().any(|i| self.is_fast(h, i).is_none()) {
                        found.push(*e);
                    }
                }
                changed |= !found.is_empty();
                self.yields.extend(found);
            }
            if !changed {
                break;
            }
        }

        // The plain clauses, and the functions that discontinue performs
        // with, are functions of the module like any other.
        let ids: Vec<HandlerId> = self.src.handlers.keys().copied().collect();
        for h in ids {
            let n = self.src.handlers[&h].len();
            let mut fast = Vec::new();
            for i in 0..n {
                match self.is_fast(h, i) {
                    Some(plain) => {
                        let id = self.fresh_func();
                        self.src.funcs.insert(id, plain);
                        fast.push(Some(id));
                    }
                    None => fast.push(None),
                }
            }
            let starts = self.starts_table(h);
            let funcs = HandlerFuncs {
                starts,
                fast,
                start_body: self.fresh_func(),
                drive: self.fresh_func(),
                reprompt: self.fresh_func(),
                kont: self.fresh_func(),
            };
            self.handlers.insert(h, funcs);
        }
        // In the order they're found, so that the IDs are the same each
        // time.
        let mut discontinued: Vec<(EffectId, u32, Vec<Ty>)> = Vec::new();
        for f in self.src.funcs.values() {
            f.body.walk(|b| {
                for l in &b.lets {
                    if let (Op::Discontinue(e, i), Some(Atom::Var(k))) = (&l.op, l.args.first())
                        && let Some(Ty::Cont(sig)) = f.vars.get(k.0 as usize)
                    {
                        let key = (*e, *i, sig.params.clone());
                        if !discontinued.contains(&key) {
                            discontinued.push(key);
                        }
                    }
                }
            });
        }
        for (e, i, takes) in discontinued {
            let op = &self.src.effects[&e].ops[i as usize];
            let mut f = new_func(op.params.clone(), takes.clone());
            f.row = Row::one(e);
            // What the operation gives fits what the continuation takes.
            let rs: Vec<Var> = op.results.iter().map(|t| f.new_var(t.clone())).collect();
            let args = vs(&f.params.clone());
            f.body = block(
                vec![Let {
                    binds: rs.clone(),
                    op: Op::Perform(e, i),
                    args,
                    source: None,
                }],
                ret(vs(&rs)),
            );
            f.debug.name = Some(format!("discontinue_{}", e.0).into());
            let id = self.fresh_func();
            self.src.funcs.insert(id, f);
            self.perf.insert((e, i, takes), id);
        }

        self.find_takes_ev();
        let ids: Vec<FuncId> = self.src.funcs.keys().copied().collect();
        for id in ids {
            self.lift_joins(id);
        }
        // Lifted join points take evidence like the function they're from.
        self.find_takes_ev();
        for &f in &self.takes_ev.clone() {
            let id = self.fresh_func();
            self.internal.insert(f, id);
        }
        Ok(())
    }

    /// Which functions take evidence: those with a row, and those that
    /// apply a closure, handle, resume, discontinue, or call one that does.
    fn find_takes_ev(&mut self) {
        loop {
            let mut changed = false;
            for (&id, f) in &self.src.funcs {
                if self.takes_ev.contains(&id) {
                    continue;
                }
                let needs = !f.row.is_nothing()
                    || any_let(&f.body, &mut |l| match &l.op {
                        Op::Apply
                        | Op::Handle(..)
                        | Op::Perform(..)
                        | Op::Resume
                        | Op::Discontinue(..) => true,
                        Op::Call(g, ..) => self.takes_ev.contains(g),
                        _ => false,
                    });
                if needs {
                    self.takes_ev.insert(id);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Whether `l`, in a function with variables of types `vars`, may
    /// yield.
    fn may_yield(&self, l: &Let, vars: &[Ty]) -> bool {
        let ty = |a: &Atom| match a {
            Atom::Var(x) => vars.get(x.0 as usize).cloned(),
            Atom::Lit(_) => None,
        };
        match &l.op {
            Op::Call(g, _, rows) => self
                .src
                .funcs
                .get(g)
                .is_some_and(|f| self.row_yields(&f.row.subst(rows))),
            Op::Apply | Op::Resume | Op::Discontinue(..) => match l.args.first().and_then(ty) {
                Some(Ty::Fn(sig, _) | Ty::Cont(sig)) => self.row_yields(&sig.row),
                _ => true,
            },
            Op::Handle(h, rows) => self
                .src
                .handlers
                .get(h)
                .is_some_and(|d| self.row_yields(&d.row.subst(rows))),
            Op::Perform(e, _) => self.yields.contains(e),
            _ => false,
        }
    }

    /// Whether the `i`th `let` of `b` is a site: one that may yield and
    /// isn't a tail call.
    fn is_site(&self, b: &Block, i: usize, vars: &[Ty]) -> bool {
        conduit_ir::deep(|| {
            let l = &b.lets[i];
            self.may_yield(l, vars) && !is_tail(b, i)
        })
    }
}

/// Whether the `i`th `let` of `b` is a tail call: the last, followed by a
/// jump to `ret` with what it binds.
fn is_tail(b: &Block, i: usize) -> bool {
    let l = &b.lets[i];
    i + 1 == b.lets.len()
        && matches!(&b.tail, Tail::Jump { target, args, .. }
            if *target == Label::RET && *args == vs(&l.binds))
}

// ---------------------------------------------------------------------
// Lifting join points.

/// A lifted join point: its function and the variables it's given before
/// its parameters.
struct Lifted {
    func: FuncId,
    free: Vec<Var>,
}

impl Lower {
    /// Turns each join point of `id` that's in scope at a site into a
    /// function, and each jump to it into a tail call. A join point that
    /// one of those jumps to, but doesn't define, is lifted too, since the
    /// function can't jump back into this one.
    fn lift_joins(&mut self, id: FuncId) {
        let f = &self.src.funcs[&id];
        if !self.takes_ev.contains(&id) {
            return;
        }
        // The labels in scope at a site.
        let mut lifted: BTreeSet<Label> = BTreeSet::new();
        let mut defs: BTreeMap<Label, &JoinDef> = BTreeMap::new();
        self.scope_at_sites(&f.body, &mut Vec::new(), &f.vars, &mut lifted, &mut defs);
        if lifted.is_empty() {
            return;
        }
        loop {
            let mut more = BTreeSet::new();
            for l in &lifted {
                let d = &defs[l];
                let inner = defined_labels(&d.body);
                for j in jumped_labels(&d.body) {
                    if j != Label::RET && !inner.contains(&j) && !lifted.contains(&j) {
                        more.insert(j);
                    }
                }
            }
            if more.is_empty() {
                break;
            }
            lifted.extend(more);
        }
        // What each is given: its free variables, and those of the lifted
        // join points it jumps to, which it then gives them.
        let mut free: BTreeMap<Label, BTreeSet<Var>> = BTreeMap::new();
        for l in &lifted {
            let d = &defs[l];
            let mut fv = free_vars(&d.body);
            for p in &d.params {
                fv.remove(p);
            }
            free.insert(*l, fv);
        }
        loop {
            let mut changed = false;
            for l in lifted.iter().copied().collect::<Vec<_>>() {
                let d = &defs[&l];
                let mut fv = free[&l].clone();
                for j in jumped_labels(&d.body) {
                    if let Some(other) = free.get(&j) {
                        fv.extend(other.iter().copied());
                    }
                }
                for p in &d.params {
                    fv.remove(p);
                }
                if fv != free[&l] {
                    free.insert(l, fv);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // Only the lifted definitions are copied: each holds what's nested
        // in it, which for all of them would take as long as the
        // function's depth squared.
        let defs: BTreeMap<Label, JoinDef> =
            (lifted.iter()).map(|l| (*l, defs[l].clone())).collect();
        let mut table = BTreeMap::new();
        for &l in &lifted {
            let func = self.fresh_func();
            table.insert(
                l,
                Lifted {
                    func,
                    free: free[&l].iter().copied().collect(),
                },
            );
        }
        let f = self.src.funcs[&id].clone();
        for (&l, lifted_def) in &table {
            let d = &defs[&l];
            let mut j = f.without_body();
            j.params = lifted_def.free.clone();
            j.params.extend(d.params.iter().copied());
            let body = rewrite_jumps(d.body.clone(), &table, &mut j, &f);
            j.body = body;
            let label = f
                .debug
                .labels
                .get(&l)
                .map_or(l.0.to_string(), |n| n.to_string());
            let name = f
                .debug
                .name
                .as_deref()
                .map_or(id.to_string(), str::to_string);
            j.debug.name = Some(format!("{name}_{label}").into());
            self.src.funcs.insert(lifted_def.func, j);
        }
        let mut f2 = f.without_body();
        f2.body = rewrite_jumps(f.body.clone(), &table, &mut f2, &f);
        self.src.funcs.insert(id, f2);
    }

    /// Adds to `lifted` the labels in scope at each site of `b`, and to
    /// `defs` the definition of every label.
    fn scope_at_sites<'b>(
        &self,
        b: &'b Block,
        scope: &mut Vec<Label>,
        vars: &[Ty],
        lifted: &mut BTreeSet<Label>,
        defs: &mut BTreeMap<Label, &'b JoinDef>,
    ) {
        conduit_ir::deep(|| {
            for i in 0..b.lets.len() {
                if self.is_site(b, i, vars) {
                    lifted.extend(scope.iter().copied());
                }
            }
            match &b.tail {
                Tail::Join {
                    rec,
                    defs: ds,
                    body,
                } => {
                    for d in ds {
                        defs.insert(d.label, d);
                    }
                    let n = scope.len();
                    if *rec {
                        scope.extend(ds.iter().map(|d| d.label));
                    }
                    for d in ds {
                        self.scope_at_sites(&d.body, scope, vars, lifted, defs);
                    }
                    scope.truncate(n);
                    scope.extend(ds.iter().map(|d| d.label));
                    self.scope_at_sites(body, scope, vars, lifted, defs);
                    scope.truncate(n);
                }
                Tail::Case { arms, default, .. } => {
                    for a in arms {
                        self.scope_at_sites(&a.body, scope, vars, lifted, defs);
                    }
                    if let Some(d) = default {
                        self.scope_at_sites(d, scope, vars, lifted, defs);
                    }
                }
                Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
            }
        })
    }
}

/// `b` with each jump to a lifted join point a tail call of its function,
/// and the lifted definitions gone. `into` is the function `b` goes in,
/// which gets the variables for the calls' results, and `orig` the one the
/// join points are from.
fn rewrite_jumps(b: Block, table: &BTreeMap<Label, Lifted>, into: &mut Func, orig: &Func) -> Block {
    conduit_ir::deep(|| {
        let mut lets = b.lets;
        let tail = match b.tail {
            Tail::Jump {
                target,
                args,
                source,
            } => match table.get(&target) {
                Some(lifted) => {
                    let rows: Vec<Row> = (1..=orig.effect_params).map(Row::param).collect();
                    let mut all = vs(&lifted.free);
                    all.extend(args);
                    let rs = letn(
                        into,
                        &mut lets,
                        orig.results.clone(),
                        Op::Call(lifted.func, same_margs(orig), rows),
                        all,
                    );
                    Tail::Jump {
                        target: Label::RET,
                        args: vs(&rs),
                        source,
                    }
                }
                None => Tail::Jump {
                    target,
                    args,
                    source,
                },
            },
            Tail::Join { rec, defs, body } => {
                let defs: Vec<JoinDef> = defs
                    .into_iter()
                    .filter(|d| !table.contains_key(&d.label))
                    .map(|d| JoinDef {
                        body: rewrite_jumps(d.body, table, into, orig),
                        ..d
                    })
                    .collect();
                let body = rewrite_jumps(*body, table, into, orig);
                if defs.is_empty() {
                    lets.extend(body.lets);
                    return block(lets, body.tail);
                }
                Tail::Join {
                    rec,
                    defs,
                    body: Box::new(body),
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
                    .map(|a| Arm {
                        tag: a.tag,
                        body: rewrite_jumps(a.body, table, into, orig),
                    })
                    .collect(),
                default: default.map(|d| Box::new(rewrite_jumps(*d, table, into, orig))),
            },
            Tail::Unreachable => Tail::Unreachable,
            Tail::Trap => Tail::Trap,
        };
        block(lets, tail)
    })
}

// ---------------------------------------------------------------------
// Types and the runtime.

impl Lower {
    /// A type after lowering: no rows, closures and continuations taking
    /// evidence last, and a continuation a closure that's given what the
    /// `perform` gives as a closure.
    fn ty(&self, t: &Ty) -> Ty {
        match t {
            Ty::Data(d, m, args, _) => Ty::Data(*d, *m, args.clone(), Vec::new()),
            Ty::Array(e, m) => Ty::Array(Box::new(self.ty(e)), *m),
            Ty::Fn(sig, m) => {
                let mut params = self.tys(&sig.params);
                params.push(self.rt.ev_ty());
                Ty::Fn(Box::new(Sig::new(params, self.tys(&sig.results))), *m)
            }
            Ty::Cont(sig) => self.cont_ty(&self.tys(&sig.results)),
            Ty::Ref(t) => Ty::Ref(Box::new(self.ty(t))),
            other => other.clone(),
        }
    }

    fn tys(&self, ts: &[Ty]) -> Vec<Ty> {
        ts.iter().map(|t| self.ty(t)).collect()
    }

    /// A continuation that gives `results`: `fn(thunk, ev) -> results`.
    fn cont_ty(&self, results: &[Ty]) -> Ty {
        Ty::Fn(
            Box::new(Sig::new(vec![any(), self.rt.ev_ty()], results.to_vec())),
            Mutability::Mut,
        )
    }

    /// What a thunk is: `fn(ev) -> results`.
    fn thunk_ty(&self, results: &[Ty]) -> Ty {
        Ty::Fn(
            Box::new(Sig::new(vec![self.rt.ev_ty()], results.to_vec())),
            Mutability::Mut,
        )
    }

    /// A new output function taking `params` and the evidence, with
    /// the variable of the evidence.
    fn ev_func(&self, params: Vec<Ty>, results: Vec<Ty>, name: &str) -> (Func, Var) {
        let mut all = params;
        all.push(self.rt.ev_ty());
        let mut f = new_func(all, results);
        f.debug.name = Some(name.into());
        let ev = *f.params.last().unwrap();
        (f, ev)
    }

    /// The tuple that boxes values of types `tys`, in a function with
    /// `params` mutability parameters, whose types they may use.
    fn tuple(&mut self, tys: &[Ty], params: u32) -> DataId {
        let key = (tys.to_vec(), params);
        if let Some(&d) = self.tuples.get(&key) {
            return d;
        }
        let d = self.fresh_data();
        self.out.data.insert(
            d,
            DataDecl {
                params,
                effect_params: 0,
                ctors: vec![CtorDecl {
                    fields: tys.to_vec(),
                }],
                debug_name: Some(format!("Tuple{}", tys.len()).into()),
            },
        );
        self.tuples.insert(key, d);
        d
    }

    /// Boxes `vals` of types `tys` into one `ro any`: none as a box of 0,
    /// one as itself, several in a tuple.
    fn box_vals(&mut self, f: &mut Func, lets: &mut Vec<Let>, vals: Vec<Atom>, tys: &[Ty]) -> Atom {
        let (ty, val) = match vals.len() {
            0 => (Ty::I32, int(0)),
            1 => (tys[0].clone(), vals[0].clone()),
            _ => {
                let d = self.tuple(tys, f.mut_params.count);
                let t = Ty::Data(d, Mutability::Mut, same_margs(f), Vec::new());
                let x = let1(f, lets, t.clone(), Op::New(Runtime::ctor(d, 0)), vals);
                (t, v(x))
            }
        };
        v(let1(f, lets, any(), Op::Box(ty), vec![val]))
    }

    /// The values of types `tys` that [`box_vals`](Self::box_vals) boxed
    /// in `x`, bound to `binds`.
    fn unbox_vals(
        &mut self,
        f: &mut Func,
        lets: &mut Vec<Let>,
        x: Atom,
        tys: &[Ty],
        binds: Vec<Var>,
    ) {
        match tys.len() {
            0 => {}
            1 => lets.push(Let {
                binds,
                op: Op::Unbox(tys[0].clone()),
                args: vec![x],
                source: None,
            }),
            _ => {
                let d = self.tuple(tys, f.mut_params.count);
                let t = Ty::Data(d, Mutability::Mut, same_margs(f), Vec::new());
                let tup = let1(
                    f,
                    lets,
                    t,
                    Op::Unbox(Ty::Data(d, Mutability::Mut, same_margs(f), Vec::new())),
                    vec![x],
                );
                for (i, b) in binds.into_iter().enumerate() {
                    lets.push(Let {
                        binds: vec![b],
                        op: Op::Get(Runtime::ctor(d, 0), i as u32),
                        args: vec![v(tup)],
                        source: None,
                    });
                }
            }
        }
    }

    /// Whether the code is yielding: an `i32`.
    fn yielding(&self, f: &mut Func, lets: &mut Vec<Let>, ev: Atom) -> Var {
        let st = let1(
            f,
            lets,
            self.rt.yield_ty(),
            Op::Get(Runtime::ctor(self.rt.ev_d, 0), 0),
            vec![ev],
        );
        let mk = let1(
            f,
            lets,
            Ty::I32,
            Op::Get(Runtime::ctor(self.rt.yield_d, 0), 0),
            vec![v(st)],
        );
        let1(f, lets, Ty::I32, Op::Prim(Prim::Ne), vec![v(mk), int(0)])
    }

    /// Adds `piece` to the continuation being built.
    fn extend(&self, f: &mut Func, lets: &mut Vec<Let>, ev: Atom, piece: Atom) {
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let st = let1(
            f,
            lets,
            self.rt.yield_ty(),
            Op::Get(Runtime::ctor(self.rt.ev_d, 0), 0),
            vec![ev],
        );
        let cur = let1(f, lets, self.rt.piece_ty(), Op::Get(yc, 3), vec![v(st)]);
        let c = let1(
            f,
            lets,
            self.rt.piece_ty(),
            Op::Closure(self.rt.compose, Vec::new(), Vec::new()),
            vec![v(cur), piece],
        );
        let0(lets, Op::Set(yc, 3), vec![v(st), v(c)]);
    }

    /// `undefined` values of types `tys`.
    fn undefineds(&self, f: &mut Func, lets: &mut Vec<Let>, tys: &[Ty]) -> Vec<Atom> {
        tys.iter()
            .map(|t| v(let1(f, lets, t.clone(), Op::Undefined, Vec::new())))
            .collect()
    }

    /// `ret` with an undefined `ro any`.
    fn ret_undefined_any(&self, f: &mut Func, lets: &mut Vec<Let>) -> Tail {
        conduit_ir::deep(|| {
            let u = self.undefineds(f, lets, &[any()]);
            ret(u)
        })
    }

    /// The runtime's own functions.
    fn runtime_funcs(&mut self) {
        let rt_hs = Runtime::ctor(self.rt.hs_d, 1);
        let ev_c = Runtime::ctor(self.rt.ev_d, 0);

        // lookup(ev, effect) -> (node, start)
        let node_c = self.rt.node();
        let mut f = new_func(
            vec![self.rt.ev_ty(), Ty::I32],
            vec![self.rt.node_ty(), Ty::I32],
        );
        f.debug.name = Some("lookup".into());
        let [ev, e] = [f.params[0], f.params[1]];
        let mut lets = Vec::new();
        let first = let1(
            &mut f,
            &mut lets,
            self.rt.hs_ty(),
            Op::Get(ev_c, 1),
            vec![v(ev)],
        );
        let hs = f.new_var(self.rt.hs_ty());
        let lp = f.new_label();
        let mut found = Vec::new();
        let node = let1(
            &mut f,
            &mut found,
            self.rt.node_ty(),
            Op::Get(rt_hs, 0),
            vec![v(hs)],
        );
        let starts = let1(
            &mut f,
            &mut found,
            Ty::Array(Box::new(Ty::I32), Mutability::Imm),
            Op::Get(node_c, 0),
            vec![v(node)],
        );
        let start = let1(
            &mut f,
            &mut found,
            Ty::I32,
            Op::ArrayGet,
            vec![v(starts), v(e)],
        );
        let handles = let1(
            &mut f,
            &mut found,
            Ty::I32,
            Op::Prim(Prim::Ge(conduit_ir::Sign::Signed)),
            vec![v(start), int(0)],
        );
        let mut next_lets = Vec::new();
        let outer = let1(
            &mut f,
            &mut next_lets,
            self.rt.ev_ty(),
            Op::Get(node_c, 4),
            vec![v(node)],
        );
        let next = let1(
            &mut f,
            &mut next_lets,
            self.rt.hs_ty(),
            Op::Get(ev_c, 1),
            vec![v(outer)],
        );
        let found_block = block(
            found,
            case_zero(
                handles,
                block(
                    next_lets,
                    Tail::Jump {
                        target: lp,
                        args: vec![v(next)],
                        source: None,
                    },
                ),
                block(vec![], ret(vec![v(node), v(start)])),
            ),
        );
        let lp_body = block(
            vec![],
            Tail::Case {
                scrut: v(hs),
                arms: vec![
                    // No handler: an unhandled effect stops the program.
                    Arm {
                        tag: Tag::Ctor(Runtime::ctor(self.rt.hs_d, 0)),
                        body: Tail::Trap.into(),
                    },
                    Arm {
                        tag: Tag::Ctor(rt_hs),
                        body: found_block,
                    },
                ],
                default: None,
            },
        );
        f.body = block(
            lets,
            Tail::Join {
                rec: true,
                defs: vec![JoinDef {
                    label: lp,
                    params: vec![hs],
                    body: lp_body,
                }],
                body: Box::new(
                    Tail::Jump {
                        target: lp,
                        args: vec![v(first)],
                        source: None,
                    }
                    .into(),
                ),
            },
        );
        self.out.funcs.insert(self.rt.lookup, f);

        // compose(prev, next, x, ev) -> any
        let piece = self.rt.piece_ty();
        let mut f = new_func(
            vec![piece.clone(), piece.clone(), any(), self.rt.ev_ty()],
            vec![any()],
        );
        f.debug.name = Some("compose".into());
        let [prev, next, x, ev] = [f.params[0], f.params[1], f.params[2], f.params[3]];
        let mut lets = Vec::new();
        let y = let1(
            &mut f,
            &mut lets,
            any(),
            Op::Apply,
            vec![v(prev), v(x), v(ev)],
        );
        let yy = self.yielding(&mut f, &mut lets, v(ev));
        let mut go = Vec::new();
        let r = let1(
            &mut f,
            &mut go,
            any(),
            Op::Apply,
            vec![v(next), v(y), v(ev)],
        );
        let mut stop = Vec::new();
        self.extend(&mut f, &mut stop, v(ev), v(next));
        let t = self.ret_undefined_any(&mut f, &mut stop);
        f.body = block(
            lets,
            case_zero(yy, block(go, ret(vec![v(r)])), block(stop, t)),
        );
        self.out.funcs.insert(self.rt.compose, f);

        // start_resume(cont, x, ev) -> any
        let mut f = new_func(vec![piece, any(), self.rt.ev_ty()], vec![any()]);
        f.debug.name = Some("start_resume".into());
        let [cont, x, ev] = [f.params[0], f.params[1], f.params[2]];
        let mut lets = Vec::new();
        let r = let1(
            &mut f,
            &mut lets,
            any(),
            Op::Apply,
            vec![v(cont), v(x), v(ev)],
        );
        f.body = block(lets, ret(vec![v(r)]));
        self.out.funcs.insert(self.rt.start_resume, f);
    }

    /// The closure `resume` gives a continuation: `const(vals.., ev)`,
    /// which gives the values, of types `tys`.
    fn const_func(&mut self, tys: &[Ty]) -> FuncId {
        if let Some(&id) = self.consts.get(tys) {
            return id;
        }
        let id = self.fresh_func();
        let (mut f, _) = self.ev_func(tys.to_vec(), tys.to_vec(), "resume_with");
        let vals: Vec<Var> = f.params[..tys.len()].to_vec();
        f.body = ret(vs(&vals)).into();
        self.out.funcs.insert(id, f);
        self.consts.insert(tys.to_vec(), id);
        id
    }

    /// `force_E_i(next, site_ev, x, ev) -> any`: the first piece of the
    /// continuation of a `perform E.i`. It calls the closure in `x`, which
    /// gives what the `perform` gives, at the `perform`, then `next`, the
    /// rest of the `perform`'s function.
    fn force_func(&mut self, e: EffectId, i: u32) -> FuncId {
        if let Some(&id) = self.force.get(&(e, i)) {
            return id;
        }
        let id = self.fresh_func();
        let results = self.tys(&self.src.effects[&e].ops[i as usize].results);
        let piece = self.rt.piece_ty();
        let mut f = new_func(
            vec![piece, self.rt.ev_ty(), any(), self.rt.ev_ty()],
            vec![any()],
        );
        f.debug.name = Some("force".into());
        let [next, site_ev, x] = [f.params[0], f.params[1], f.params[2]];
        let mut lets = Vec::new();
        let t = let1(
            &mut f,
            &mut lets,
            self.thunk_ty(&results),
            Op::Unbox(self.thunk_ty(&results)),
            vec![v(x)],
        );
        let xs = letn(
            &mut f,
            &mut lets,
            results.clone(),
            Op::Apply,
            vec![v(t), v(site_ev)],
        );
        let y = self.yielding(&mut f, &mut lets, v(site_ev));
        let mut go = Vec::new();
        let b = self.box_vals(&mut f, &mut go, vs(&xs), &results);
        let r = let1(
            &mut f,
            &mut go,
            any(),
            Op::Apply,
            vec![v(next), b, v(site_ev)],
        );
        let mut stop = Vec::new();
        self.extend(&mut f, &mut stop, v(site_ev), v(next));
        let tail = self.ret_undefined_any(&mut f, &mut stop);
        f.body = block(
            lets,
            case_zero(y, block(go, ret(vec![v(r)])), block(stop, tail)),
        );
        self.out.funcs.insert(id, f);
        self.force.insert((e, i), id);
        id
    }

    /// The function a closure of `g` calls: `g` itself if it takes
    /// evidence, or one that takes it and calls `g`.
    fn ev_version(&mut self, g: FuncId) -> FuncId {
        if let Some(&id) = self.internal.get(&g) {
            return id;
        }
        if let Some(&id) = self.ev_wrappers.get(&g) {
            return id;
        }
        let src = self.src.funcs[&g].without_body();
        let id = self.fresh_func();
        let params = self.tys(&src.sig().params);
        let results = self.tys(&src.results);
        let name = src
            .debug
            .name
            .as_deref()
            .map_or(g.to_string(), str::to_string);
        let (mut f, _) = self.ev_func(params.clone(), results.clone(), &format!("{name}_ev"));
        f.mut_params = src.mut_params.clone();
        let args = vs(&f.params[..params.len()]);
        let mut lets = Vec::new();
        let rs = letn(
            &mut f,
            &mut lets,
            results,
            Op::Call(g, same_margs(&src), Vec::new()),
            args,
        );
        f.body = block(lets, ret(vs(&rs)));
        self.out.funcs.insert(id, f);
        self.ev_wrappers.insert(g, id);
        id
    }
}

// ---------------------------------------------------------------------
// Handlers.

impl Lower {
    /// What a `handle` of `h` gives, its body gives, and its environment,
    /// after lowering.
    fn handler_types(&self, h: HandlerId) -> (Vec<Ty>, Vec<Ty>, Vec<Ty>) {
        let decl = &self.src.handlers[&h];
        let ret = &self.src.funcs[&decl.ret];
        let sig = ret.sig();
        let env = self.tys(&decl.env);
        let body = self.tys(&sig.params[decl.env.len()..]);
        (self.tys(&ret.results), body, env)
    }

    /// The call of clause or return clause `clause` with `args`, passing
    /// `ev` if it takes evidence.
    fn clause_call(&self, clause: FuncId, mut args: Vec<Atom>, ev: Var) -> (Op, Vec<Atom>) {
        let id = match self.internal.get(&clause) {
            Some(&id) => {
                args.push(v(ev));
                id
            }
            None => clause,
        };
        (Op::Call(id, Vec::new(), Vec::new()), args)
    }

    fn handler_funcs(&mut self, h: HandlerId) {
        let (results, body_results, env_tys) = self.handler_types(h);
        let decl = self.src.handlers[&h].clone();
        let funcs = &self.handlers[&h];
        let (start_body, drive, reprompt, kont) =
            (funcs.start_body, funcs.drive, funcs.reprompt, funcs.kont);
        let name = decl
            .debug_name
            .as_deref()
            .map_or(h.to_string(), str::to_string);
        let hs_c = self.rt.node();
        let ev_c = Runtime::ctor(self.rt.ev_d, 0);
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let ev_ty = self.rt.ev_ty();
        let n_env = env_tys.len();

        // start_body(body, ev) -> any
        let body_ty = Ty::Fn(
            Box::new(Sig::new(vec![ev_ty.clone()], body_results.clone())),
            Mutability::ReadOnly,
        );
        let mut f = new_func(vec![body_ty, ev_ty.clone()], vec![any()]);
        f.debug.name = Some(format!("{name}_start").into());
        let [b, ev] = [f.params[0], f.params[1]];
        let mut lets = Vec::new();
        let rs = letn(
            &mut f,
            &mut lets,
            body_results.clone(),
            Op::Apply,
            vec![v(b), v(ev)],
        );
        let y = self.yielding(&mut f, &mut lets, v(ev));
        let mut go = Vec::new();
        let bx = self.box_vals(&mut f, &mut go, vs(&rs), &body_results);
        let mut stop = Vec::new();
        let t = self.ret_undefined_any(&mut f, &mut stop);
        f.body = block(lets, case_zero(y, block(go, ret(vec![bx])), block(stop, t)));
        self.out.funcs.insert(start_body, f);

        // drive(ev, env.., node, start) -> R
        let mut params = vec![ev_ty.clone()];
        params.extend(env_tys.iter().cloned());
        params.push(self.rt.node_ty());
        params.push(self.rt.start_ty());
        let mut f = new_func(params, results.clone());
        f.debug.name = Some(format!("{name}_drive").into());
        let ev_outer = f.params[0];
        let env: Vec<Var> = f.params[1..1 + n_env].to_vec();
        let node = f.params[1 + n_env];
        let start = f.params[2 + n_env];
        let mut lets = Vec::new();
        let0(&mut lets, Op::Set(hs_c, 4), vec![v(node), v(ev_outer)]);
        let st = let1(
            &mut f,
            &mut lets,
            self.rt.yield_ty(),
            Op::Get(ev_c, 0),
            vec![v(ev_outer)],
        );
        let hs = let1(
            &mut f,
            &mut lets,
            self.rt.hs_ty(),
            Op::New(Runtime::ctor(self.rt.hs_d, 1)),
            vec![v(node)],
        );
        let ev_in = let1(
            &mut f,
            &mut lets,
            ev_ty.clone(),
            Op::New(ev_c),
            vec![v(st), v(hs)],
        );
        let r = let1(
            &mut f,
            &mut lets,
            any(),
            Op::Apply,
            vec![v(start), v(ev_in)],
        );
        let mk = let1(&mut f, &mut lets, Ty::I32, Op::Get(yc, 0), vec![v(st)]);
        // The body returned.
        let mut done = Vec::new();
        let bs: Vec<Var> = body_results.iter().map(|t| f.new_var(t.clone())).collect();
        self.unbox_vals(&mut f, &mut done, v(r), &body_results, bs.clone());
        let mut args = vs(&env);
        args.extend(vs(&bs));
        let (op, args) = self.clause_call(decl.ret, args, ev_outer);
        let out = letn(&mut f, &mut done, results.clone(), op, args);
        let done = block(done, ret(vs(&out)));
        // Yielding to another handler: resuming the continuation must run
        // under this one again.
        let mut other = Vec::new();
        let inner = let1(
            &mut f,
            &mut other,
            self.rt.piece_ty(),
            Op::Get(yc, 3),
            vec![v(st)],
        );
        let mut caps = vs(&env);
        caps.push(v(node));
        caps.push(v(inner));
        let w = let1(
            &mut f,
            &mut other,
            self.rt.piece_ty(),
            Op::Closure(reprompt, Vec::new(), Vec::new()),
            caps,
        );
        let0(&mut other, Op::Set(yc, 3), vec![v(st), v(w)]);
        let us = self.undefineds(&mut f, &mut other, &results);
        let other = block(other, ret(us));
        // Yielding to this one: run the clause with the continuation.
        let mut mine = Vec::new();
        let op_n = let1(&mut f, &mut mine, Ty::I32, Op::Get(yc, 1), vec![v(st)]);
        let args_any = let1(&mut f, &mut mine, any(), Op::Get(yc, 2), vec![v(st)]);
        let cont = let1(
            &mut f,
            &mut mine,
            self.rt.piece_ty(),
            Op::Get(yc, 3),
            vec![v(st)],
        );
        let0(&mut mine, Op::Set(yc, 0), vec![v(st), int(0)]);
        // The state lets go of what it held for this yield. Otherwise it
        // keeps the continuation, whose closures hold evidence that points
        // back at the state: a cycle, which reference counting can't free,
        // and which keeps everything the continuation holds alive.
        let no_args = let1(&mut f, &mut mine, any(), Op::Undefined, Vec::new());
        let0(&mut mine, Op::Set(yc, 2), vec![v(st), v(no_args)]);
        let no_cont = let1(
            &mut f,
            &mut mine,
            self.rt.piece_ty(),
            Op::Undefined,
            Vec::new(),
        );
        let0(&mut mine, Op::Set(yc, 3), vec![v(st), v(no_cont)]);
        let used = let1(
            &mut f,
            &mut mine,
            Ty::Ref(Box::new(Ty::I32)),
            Op::RefNew,
            vec![int(0)],
        );
        let mut caps = vs(&env);
        caps.extend([v(node), v(cont), v(used)]);
        let k_ty = self.cont_ty(&results);
        let k = let1(
            &mut f,
            &mut mine,
            k_ty,
            Op::Closure(kont, Vec::new(), Vec::new()),
            caps,
        );
        let mut arms = Vec::new();
        for (i, (e, j, clause)) in decl.clauses().enumerate() {
            if self.handlers[&h].fast[i].is_some() {
                continue;
            }
            let op = &self.src.effects[&e].ops[j as usize];
            let ptys = self.tys(&op.params);
            let mut arm = Vec::new();
            let ps: Vec<Var> = ptys.iter().map(|t| f.new_var(t.clone())).collect();
            self.unbox_vals(&mut f, &mut arm, v(args_any), &ptys, ps.clone());
            let mut args = vs(&env);
            args.extend(vs(&ps));
            args.push(v(k));
            let (op, args) = self.clause_call(clause, args, ev_outer);
            let r = letn(&mut f, &mut arm, results.clone(), op, args);
            arms.push(Arm {
                tag: Tag::Int(i as i64),
                body: block(arm, ret(vs(&r))),
            });
        }
        let mine = block(
            mine,
            Tail::Case {
                scrut: v(op_n),
                arms,
                default: Some(Box::new(Tail::Unreachable.into())),
            },
        );
        let mut yielding = Vec::new();
        let my_mk = let1(
            &mut f,
            &mut yielding,
            Ty::I32,
            Op::Get(hs_c, 1),
            vec![v(node)],
        );
        let same = let1(
            &mut f,
            &mut yielding,
            Ty::I32,
            Op::Prim(Prim::Eq),
            vec![v(mk), v(my_mk)],
        );
        let yielding = block(yielding, case_zero(same, other, mine));
        f.body = block(lets, case_zero(mk, done, yielding));
        self.out.funcs.insert(drive, f);

        // reprompt(env.., node, inner, x, ev) -> any
        let mut params = env_tys.clone();
        params.extend([self.rt.node_ty(), self.rt.piece_ty(), any(), ev_ty.clone()]);
        let mut f = new_func(params, vec![any()]);
        f.debug.name = Some(format!("{name}_reprompt").into());
        let env: Vec<Var> = f.params[..n_env].to_vec();
        let [node, inner, x, ev] = [
            f.params[n_env],
            f.params[n_env + 1],
            f.params[n_env + 2],
            f.params[n_env + 3],
        ];
        let mut lets = Vec::new();
        let s = let1(
            &mut f,
            &mut lets,
            self.rt.start_ty(),
            Op::Closure(self.rt.start_resume, Vec::new(), Vec::new()),
            vec![v(inner), v(x)],
        );
        let mut args = vec![v(ev)];
        args.extend(vs(&env));
        args.extend([v(node), v(s)]);
        let rs = letn(
            &mut f,
            &mut lets,
            results.clone(),
            Op::Call(drive, Vec::new(), Vec::new()),
            args,
        );
        let y = self.yielding(&mut f, &mut lets, v(ev));
        let mut go = Vec::new();
        let bx = self.box_vals(&mut f, &mut go, vs(&rs), &results);
        let mut stop = Vec::new();
        let t = self.ret_undefined_any(&mut f, &mut stop);
        f.body = block(lets, case_zero(y, block(go, ret(vec![bx])), block(stop, t)));
        self.out.funcs.insert(reprompt, f);

        // kont(env.., node, cont, used, x, ev) -> R
        let mut params = env_tys.clone();
        params.extend([
            self.rt.node_ty(),
            self.rt.piece_ty(),
            Ty::Ref(Box::new(Ty::I32)),
            any(),
            ev_ty.clone(),
        ]);
        let mut f = new_func(params, results.clone());
        f.debug.name = Some(format!("{name}_k").into());
        let env: Vec<Var> = f.params[..n_env].to_vec();
        let [node, cont, used, x, ev] = [
            f.params[n_env],
            f.params[n_env + 1],
            f.params[n_env + 2],
            f.params[n_env + 3],
            f.params[n_env + 4],
        ];
        let mut lets = Vec::new();
        let u = let1(&mut f, &mut lets, Ty::I32, Op::RefGet, vec![v(used)]);
        let mut first = Vec::new();
        let0(&mut first, Op::RefSet, vec![v(used), int(1)]);
        let s = let1(
            &mut f,
            &mut first,
            self.rt.start_ty(),
            Op::Closure(self.rt.start_resume, Vec::new(), Vec::new()),
            vec![v(cont), v(x)],
        );
        let mut args = vec![v(ev)];
        args.extend(vs(&env));
        args.extend([v(node), v(s)]);
        let rs = letn(
            &mut f,
            &mut first,
            results.clone(),
            Op::Call(drive, Vec::new(), Vec::new()),
            args,
        );
        // A second use traps.
        f.body = block(
            lets,
            case_zero(u, block(first, ret(vs(&rs))), Tail::Trap.into()),
        );
        self.out.funcs.insert(kont, f);
    }
}

// ---------------------------------------------------------------------
// Functions.

/// A function being lowered.
struct Out<'s> {
    /// The function it's from. A rest is from the function of its site.
    src: &'s Func,
    f: Func,
    /// The variable holding the evidence, if it takes some.
    ev: Option<Var>,
    /// Whether it's a rest, which gives what its function would, boxed.
    boxed: bool,
}

impl Lower {
    fn emit(&mut self) -> Result<(), String> {
        for (&d, decl) in &self.src.data {
            let ctors = decl
                .ctors
                .iter()
                .map(|c| CtorDecl {
                    fields: self.tys(&c.fields),
                })
                .collect();
            self.out.data.insert(
                d,
                DataDecl {
                    params: decl.params,
                    effect_params: 0,
                    ctors,
                    debug_name: decl.debug_name.clone(),
                },
            );
        }
        // An extern's row only limited its callers, and no effects are left.
        self.out.externs = self.src.externs.clone();
        // An exported function keeps its ID, wrapped if it takes evidence.
        self.out.exports = self.src.exports.clone();
        for ext in self.out.externs.values_mut() {
            ext.sig.row = Row::NOTHING;
        }
        for (&c, decl) in &self.src.consts {
            let mut decl = decl.clone();
            decl.ty = self.ty(&decl.ty);
            self.out.consts.insert(c, decl);
        }
        self.runtime_funcs();
        for h in self.src.handlers.keys().copied().collect::<Vec<_>>() {
            self.handler_funcs(h);
        }
        let ids: Vec<FuncId> = self.src.funcs.keys().copied().collect();
        for id in ids {
            let src = std::rc::Rc::new(self.src.funcs[&id].clone());
            let with_ev = self.takes_ev.contains(&id);
            let (f, ev) = self.out_func(&src, with_ev);
            let mut o = Out {
                src: &src,
                f,
                ev,
                boxed: false,
            };
            o.f.body = self.block_from(&mut o, &src.body, 0);
            // Sites are found by the address of their `let`, which another
            // function's could have once this one is dropped.
            self.sites.clear();
            match self.internal.get(&id) {
                Some(&internal) => {
                    // The wrapper keeps the name.
                    if let Some(name) = &src.debug.name {
                        o.f.debug.name = Some(format!("{name}_ev").into());
                    }
                    self.out.funcs.insert(internal, o.f);
                    let wrapper = self.entry_wrapper(&src, internal);
                    self.out.funcs.insert(id, wrapper);
                }
                None => {
                    self.out.funcs.insert(id, o.f);
                }
            }
        }
        Ok(())
    }

    /// The lowered function of `src`, with its body still to fill in, and
    /// the variable of its evidence if it takes some.
    fn out_func(&self, src: &Func, with_ev: bool) -> (Func, Option<Var>) {
        let mut f = src.without_body();
        f.vars = self.tys(&src.vars);
        f.results = self.tys(&src.results);
        f.row = Row::NOTHING;
        f.effect_params = 0;
        let ev = with_ev.then(|| {
            let ev = f.new_var(self.rt.ev_ty());
            f.params.push(ev);
            ev
        });
        (f, ev)
    }

    /// The function at the old ID of `src`, which takes evidence as
    /// `internal`: it makes new evidence and calls `internal`. A yield
    /// that gets this far has no handler, which can't happen, since
    /// `perform` traps first.
    fn entry_wrapper(&self, src: &Func, internal: FuncId) -> Func {
        let params = self.tys(&src.sig().params);
        let results = self.tys(&src.results);
        let mut f = new_func(params, results.clone());
        f.mut_params = src.mut_params.clone();
        f.debug.name = src.debug.name.clone();
        let mut lets = Vec::new();
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let zero = let1(
            &mut f,
            &mut lets,
            Ty::Any(Mutability::Imm),
            Op::Box(Ty::I32),
            vec![int(0)],
        );
        let no_cont = let1(
            &mut f,
            &mut lets,
            self.rt.piece_ty(),
            Op::Undefined,
            Vec::new(),
        );
        let st = let1(
            &mut f,
            &mut lets,
            self.rt.yield_ty(),
            Op::New(yc),
            vec![int(0), int(0), v(zero), v(no_cont), int(1)],
        );
        let hs = let1(
            &mut f,
            &mut lets,
            self.rt.hs_ty(),
            Op::New(Runtime::ctor(self.rt.hs_d, 0)),
            Vec::new(),
        );
        let ev = let1(
            &mut f,
            &mut lets,
            self.rt.ev_ty(),
            Op::New(Runtime::ctor(self.rt.ev_d, 0)),
            vec![v(st), v(hs)],
        );
        let mut args = vs(&f.params.clone());
        args.push(v(ev));
        let rs = letn(
            &mut f,
            &mut lets,
            results,
            Op::Call(internal, same_margs(src), Vec::new()),
            args,
        );
        let mk = let1(&mut f, &mut lets, Ty::I32, Op::Get(yc, 0), vec![v(st)]);
        f.body = block(
            lets,
            case_zero(mk, block(vec![], ret(vs(&rs))), Tail::Unreachable.into()),
        );
        f
    }

    /// The lowered `lets[start..]` and tail of `b`.
    fn block_from(&mut self, o: &mut Out<'_>, b: &Block, start: usize) -> Block {
        conduit_ir::deep(|| {
            let mut lets = Vec::new();
            for i in start..b.lets.len() {
                let l = &b.lets[i];
                if let Op::Perform(e, op) = l.op
                    && self.yields.contains(&e)
                {
                    return self.perform_site(o, b, i, e, op, lets);
                }
                self.lower_let(o, l, &mut lets);
                if let Some(ev) = o.ev
                    && self.is_site(b, i, &o.src.vars)
                {
                    let site = self.site(o, b, i);
                    let y = self.yielding(&mut o.f, &mut lets, v(ev));
                    let go = self.continue_after(o, b, i, &site);
                    let stop = self.yield_block(o, &site);
                    return block(lets, case_zero(y, go, stop));
                }
            }
            let tail = self.lower_tail(o, &b.tail, &mut lets);
            block(lets, tail)
        })
    }

    fn lower_tail(&mut self, o: &mut Out<'_>, t: &Tail, lets: &mut Vec<Let>) -> Tail {
        conduit_ir::deep(|| match t {
            Tail::Jump {
                target,
                args,
                source,
            } if *target == Label::RET && o.boxed => {
                let tys = self.tys(&o.src.results);
                let b = self.box_vals(&mut o.f, lets, args.clone(), &tys);
                Tail::Jump {
                    target: Label::RET,
                    args: vec![b],
                    source: *source,
                }
            }
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => t.clone(),
            Tail::Join { rec, defs, body } => Tail::Join {
                rec: *rec,
                defs: defs
                    .iter()
                    .map(|d| JoinDef {
                        label: d.label,
                        params: d.params.clone(),
                        body: self.block_from(o, &d.body, 0),
                    })
                    .collect(),
                body: Box::new(self.block_from(o, body, 0)),
            },
            Tail::Case {
                scrut,
                arms,
                default,
            } => Tail::Case {
                scrut: scrut.clone(),
                arms: arms
                    .iter()
                    .map(|a| Arm {
                        tag: a.tag,
                        body: self.block_from(o, &a.body, 0),
                    })
                    .collect(),
                default: default
                    .as_deref()
                    .map(|d| Box::new(self.block_from(o, d, 0))),
            },
        })
    }

    /// What follows a site when it isn't yielding: the rest, here or, in
    /// a rest, as a tail call of the site's own rest.
    fn continue_after(&mut self, o: &mut Out<'_>, b: &Block, i: usize, site: &Site) -> Block {
        conduit_ir::deep(|| {
            if !o.boxed {
                return self.block_from(o, b, i + 1);
            }
            let ev = o.ev.expect("a rest has evidence");
            let tys: Vec<Ty> = site.binds.iter().map(|x| o.f.var_ty(*x).clone()).collect();
            let mut lets = Vec::new();
            let bx = self.box_vals(&mut o.f, &mut lets, vs(&site.binds), &tys);
            let mut args = vs(&site.free);
            args.extend([v(ev), bx, v(ev)]);
            let margs = same_margs(&o.f);
            let r = let1(
                &mut o.f,
                &mut lets,
                any(),
                Op::Call(site.rest, margs, Vec::new()),
                args,
            );
            block(lets, ret(vec![v(r)]))
        })
    }

    /// A site's way out when yielding: the rest goes on the continuation,
    /// and undefined results back.
    fn yield_block(&mut self, o: &mut Out<'_>, site: &Site) -> Block {
        conduit_ir::deep(|| {
            let ev = o.ev.expect("a site has evidence");
            let mut lets = Vec::new();
            let piece = self.rest_closure(o, &mut lets, site);
            self.extend(&mut o.f, &mut lets, v(ev), v(piece));
            let tail = self.undefined_ret(o, &mut lets);
            block(lets, tail)
        })
    }

    /// `ret` with undefined results of the function, or an undefined box
    /// in a rest.
    fn undefined_ret(&self, o: &mut Out<'_>, lets: &mut Vec<Let>) -> Tail {
        conduit_ir::deep(|| {
            if o.boxed {
                return self.ret_undefined_any(&mut o.f, lets);
            }
            let tys = o.f.results.clone();
            let us = self.undefineds(&mut o.f, lets, &tys);
            ret(us)
        })
    }

    /// A closure of a site's rest, holding what it needs.
    fn rest_closure(&mut self, o: &mut Out<'_>, lets: &mut Vec<Let>, site: &Site) -> Var {
        let ev = o.ev.expect("a site has evidence");
        let mut caps = vs(&site.free);
        caps.push(v(ev));
        let margs = same_margs(&o.f);
        let1(
            &mut o.f,
            lets,
            self.rt.piece_ty(),
            Op::Closure(site.rest, margs, Vec::new()),
            caps,
        )
    }

    /// The site at the `i`th `let` of `b`, with its rest, made the first
    /// time it's asked for.
    fn site(&mut self, o: &Out<'_>, b: &Block, i: usize) -> Site {
        conduit_ir::deep(|| {
            let key = &b.lets[i] as *const Let;
            if let Some(site) = self.sites.get(&key) {
                return site.clone();
            }
            let binds = b.lets[i].binds.clone();
            let mut free = free_vars_of(&b.lets[i + 1..], &b.tail);
            for x in &binds {
                free.remove(x);
            }
            let free: Vec<Var> = free.into_iter().collect();
            let rest = self.fresh_func();
            let site = Site {
                free: free.clone(),
                binds: binds.clone(),
                rest,
            };
            self.sites.insert(key, site.clone());

            // rest(free.., ev, x, ev') -> any
            let mut f = o.src.without_body();
            f.vars = self.tys(&o.src.vars);
            f.results = vec![any()];
            f.row = Row::NOTHING;
            f.effect_params = 0;
            f.params = free;
            let ev = f.new_var(self.rt.ev_ty());
            let x = f.new_var(any());
            let ev2 = f.new_var(self.rt.ev_ty());
            f.params.extend([ev, x, ev2]);
            let name = o.src.debug.name.as_deref().unwrap_or("fn");
            f.debug.name = Some(format!("{name}_rest").into());
            let mut r = Out {
                src: o.src,
                f,
                ev: Some(ev),
                boxed: true,
            };
            let tys: Vec<Ty> = binds.iter().map(|b| r.f.var_ty(*b).clone()).collect();
            let mut lets = Vec::new();
            self.unbox_vals(&mut r.f, &mut lets, v(x), &tys, binds);
            let mut body = self.block_from(&mut r, b, i + 1);
            lets.append(&mut body.lets);
            r.f.body = block(lets, body.tail);
            self.out.funcs.insert(rest, r.f);
            site
        })
    }

    /// `perform E.op(args)` at the `i`th `let` of `b`, of an effect that
    /// may yield: a direct call if the nearest handler's clause is
    /// tail-resumptive, or else a yield to it.
    fn perform_site(
        &mut self,
        o: &mut Out<'_>,
        b: &Block,
        i: usize,
        e: EffectId,
        op: u32,
        mut lets: Vec<Let>,
    ) -> Block {
        conduit_ir::deep(|| {
            let l = &b.lets[i];
            let ev = o.ev.expect("a perform has evidence");
            let (node, slot) = self.lookup(o, &mut lets, e, op);
            let kinds = let1(
                &mut o.f,
                &mut lets,
                Ty::Array(Box::new(Ty::I32), Mutability::Mut),
                Op::Get(self.rt.node(), 2),
                vec![v(node)],
            );
            let kind = let1(
                &mut o.f,
                &mut lets,
                Ty::I32,
                Op::ArrayGet,
                vec![v(kinds), v(slot)],
            );
            let after = o.f.new_label();
            let site = self.site(o, b, i);
            let after_body = if self.is_site(b, i, &o.src.vars) {
                let mut alets = Vec::new();
                let y = self.yielding(&mut o.f, &mut alets, v(ev));
                let go = self.continue_after(o, b, i, &site);
                let stop = self.yield_block(o, &site);
                block(alets, case_zero(y, go, stop))
            } else {
                self.block_from(o, b, i + 1)
            };
            // Tail-resumptive: call the clause under the handlers outside it.
            let mut fast = Vec::new();
            let xs = self.fast_call(o, &mut fast, (node, slot), (e, op), l.args.clone(), None);
            let fast = block(
                fast,
                Tail::Jump {
                    target: after,
                    args: vs(&xs),
                    source: l.source,
                },
            );
            // Otherwise yield to it.
            let mut slow = Vec::new();
            let yc = Runtime::ctor(self.rt.yield_d, 0);
            let st = let1(
                &mut o.f,
                &mut slow,
                self.rt.yield_ty(),
                Op::Get(Runtime::ctor(self.rt.ev_d, 0), 0),
                vec![v(ev)],
            );
            let mk = let1(
                &mut o.f,
                &mut slow,
                Ty::I32,
                Op::Get(self.rt.node(), 1),
                vec![v(node)],
            );
            let0(&mut slow, Op::Set(yc, 0), vec![v(st), v(mk)]);
            let0(&mut slow, Op::Set(yc, 1), vec![v(st), v(slot)]);
            let ptys = self.tys(&self.src.effects[&e].ops[op as usize].params);
            let a = self.box_vals(&mut o.f, &mut slow, l.args.clone(), &ptys);
            let0(&mut slow, Op::Set(yc, 2), vec![v(st), a]);
            let piece = self.rest_closure(o, &mut slow, &site);
            let force = self.force_func(e, op);
            let fz = let1(
                &mut o.f,
                &mut slow,
                self.rt.piece_ty(),
                Op::Closure(force, Vec::new(), Vec::new()),
                vec![v(piece), v(ev)],
            );
            let0(&mut slow, Op::Set(yc, 3), vec![v(st), v(fz)]);
            let tail = self.undefined_ret(o, &mut slow);
            let slow = block(slow, tail);
            block(
                lets,
                Tail::Join {
                    rec: false,
                    defs: vec![JoinDef {
                        label: after,
                        params: l.binds.clone(),
                        body: after_body,
                    }],
                    body: Box::new(block(vec![], case_zero(kind, slow, fast))),
                },
            )
        })
    }

    /// The nearest handler node of `e`, and where the clause for `e.op` is
    /// among its handler's.
    fn lookup(&self, o: &mut Out<'_>, lets: &mut Vec<Let>, e: EffectId, op: u32) -> (Var, Var) {
        let ev = o.ev.expect("a perform has evidence");
        let n = self.effect_number(e);
        let [node, start] = letn(
            &mut o.f,
            lets,
            vec![self.rt.node_ty(), Ty::I32],
            Op::Call(self.rt.lookup, Vec::new(), Vec::new()),
            vec![v(ev), int(n)],
        )[..] else {
            unreachable!()
        };
        let slot = let1(
            &mut o.f,
            lets,
            Ty::I32,
            Op::Prim(Prim::Add),
            vec![v(start), int(op as i32)],
        );
        (node, slot)
    }

    /// The type of a tail-resumptive clause's closure for `e.op`.
    fn fast_ty(&self, e: EffectId, op: u32) -> Ty {
        let o = &self.src.effects[&e].ops[op as usize];
        let mut params = self.tys(&o.params);
        params.push(self.rt.ev_ty());
        Ty::Fn(
            Box::new(Sig::new(params, self.tys(&o.results))),
            Mutability::Mut,
        )
    }

    /// Calls the tail-resumptive clause of `node` for `e.op`, at `slot`
    /// among its handler's, with `args`, under the handlers outside it.
    /// Gives the results.
    fn fast_call(
        &self,
        o: &mut Out<'_>,
        lets: &mut Vec<Let>,
        (node, slot): (Var, Var),
        (e, op): (EffectId, u32),
        mut args: Vec<Atom>,
        binds: Option<Vec<Var>>,
    ) -> Vec<Var> {
        let hs_c = self.rt.node();
        let arr = let1(
            &mut o.f,
            lets,
            Ty::Array(Box::new(any()), Mutability::Mut),
            Op::Get(hs_c, 3),
            vec![v(node)],
        );
        let c_any = let1(&mut o.f, lets, any(), Op::ArrayGet, vec![v(arr), v(slot)]);
        let fty = self.fast_ty(e, op);
        let c = let1(&mut o.f, lets, fty.clone(), Op::Unbox(fty), vec![v(c_any)]);
        let outer = let1(
            &mut o.f,
            lets,
            self.rt.ev_ty(),
            Op::Get(hs_c, 4),
            vec![v(node)],
        );
        let results = self.tys(&self.src.effects[&e].ops[op as usize].results);
        args.insert(0, v(c));
        args.push(v(outer));
        match binds {
            Some(binds) => {
                lets.push(Let {
                    binds: binds.clone(),
                    op: Op::Apply,
                    args,
                    source: None,
                });
                binds
            }
            None => letn(&mut o.f, lets, results, Op::Apply, args),
        }
    }

    /// A `let` other than a `perform` that may yield.
    fn lower_let(&mut self, o: &mut Out<'_>, l: &Let, lets: &mut Vec<Let>) {
        let ev = o.ev;
        let with_ev = |mut args: Vec<Atom>| {
            args.push(v(ev.expect("evidence for an op that needs it")));
            args
        };
        let (op, args) = match &l.op {
            Op::Call(g, margs, _) => match self.internal.get(g) {
                Some(&id) => (
                    Op::Call(id, margs.clone(), Vec::new()),
                    with_ev(l.args.clone()),
                ),
                None => (Op::Call(*g, margs.clone(), Vec::new()), l.args.clone()),
            },
            Op::Closure(g, margs, _) => {
                let id = self.ev_version(*g);
                (Op::Closure(id, margs.clone(), Vec::new()), l.args.clone())
            }
            Op::Apply => (Op::Apply, with_ev(l.args.clone())),
            Op::Perform(e, op) => {
                // Of an effect that never yields: always tail-resumptive.
                let node = self.lookup(o, lets, *e, *op);
                self.fast_call(
                    o,
                    lets,
                    node,
                    (*e, *op),
                    l.args.clone(),
                    Some(l.binds.clone()),
                );
                return;
            }
            Op::Handle(h, _) => return self.handle_site(o, l, lets, *h),
            Op::Resume => {
                let (k, vals) = l.args.split_first().expect("resume of a continuation");
                let takes = self.cont_takes(o, k);
                let cf = self.const_func(&takes);
                let t = let1(
                    &mut o.f,
                    lets,
                    self.thunk_ty(&takes),
                    Op::Closure(cf, Vec::new(), Vec::new()),
                    vals.to_vec(),
                );
                let bx = let1(
                    &mut o.f,
                    lets,
                    any(),
                    Op::Box(self.thunk_ty(&takes)),
                    vec![v(t)],
                );
                (Op::Apply, with_ev(vec![k.clone(), v(bx)]))
            }
            Op::Discontinue(e, op) => {
                let (k, args) = l.args.split_first().expect("discontinue of a continuation");
                let src_takes = match k {
                    Atom::Var(x) => match o.src.var_ty(*x) {
                        Ty::Cont(sig) => sig.params.clone(),
                        _ => Vec::new(),
                    },
                    Atom::Lit(_) => Vec::new(),
                };
                let takes = self.tys(&src_takes);
                let perf = self.perf[&(*e, *op, src_takes)];
                let id = self.ev_version(perf);
                let t = let1(
                    &mut o.f,
                    lets,
                    self.thunk_ty(&takes),
                    Op::Closure(id, Vec::new(), Vec::new()),
                    args.to_vec(),
                );
                let bx = let1(
                    &mut o.f,
                    lets,
                    any(),
                    Op::Box(self.thunk_ty(&takes)),
                    vec![v(t)],
                );
                (Op::Apply, with_ev(vec![k.clone(), v(bx)]))
            }
            Op::Box(t) => (Op::Box(self.ty(t)), l.args.clone()),
            Op::Unbox(t) => (Op::Unbox(self.ty(t)), l.args.clone()),
            op => (op.clone(), l.args.clone()),
        };
        lets.push(Let {
            binds: l.binds.clone(),
            op,
            args,
            source: l.source,
        });
    }

    /// What continuation `k` takes, after lowering.
    fn cont_takes(&self, o: &Out<'_>, k: &Atom) -> Vec<Ty> {
        match k {
            Atom::Var(x) => match o.src.var_ty(*x) {
                Ty::Cont(sig) => self.tys(&sig.params),
                _ => Vec::new(),
            },
            Atom::Lit(_) => Vec::new(),
        }
    }

    /// `handle h(env.., body)`: a node for `h` with a fresh marker, then
    /// `drive`.
    fn handle_site(&mut self, o: &mut Out<'_>, l: &Let, lets: &mut Vec<Let>, h: HandlerId) {
        let ev = o.ev.expect("a handle has evidence");
        let (env, body) = l.args.split_at(l.args.len() - 1);
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let st = let1(
            &mut o.f,
            lets,
            self.rt.yield_ty(),
            Op::Get(Runtime::ctor(self.rt.ev_d, 0), 0),
            vec![v(ev)],
        );
        let mk = let1(&mut o.f, lets, Ty::I32, Op::Get(yc, 4), vec![v(st)]);
        let next = let1(
            &mut o.f,
            lets,
            Ty::I32,
            Op::Prim(Prim::Add),
            vec![v(mk), int(1)],
        );
        let0(lets, Op::Set(yc, 4), vec![v(st), v(next)]);
        let ops: Vec<(EffectId, u32)> = self.src.handlers[&h]
            .clauses()
            .map(|(e, j, _)| (e, j))
            .collect();
        let fast: Vec<Option<FuncId>> = self.handlers[&h].fast.clone();
        let kinds: Vec<Atom> = fast.iter().map(|f| int(i32::from(f.is_some()))).collect();
        let kinds = let1(
            &mut o.f,
            lets,
            Ty::Array(Box::new(Ty::I32), Mutability::Mut),
            Op::ArrayNew,
            kinds,
        );
        let mut closures = Vec::new();
        for (i, plain) in fast.iter().enumerate() {
            let b = match plain {
                Some(plain) => {
                    let id = self.ev_version(*plain);
                    let (e, j) = ops[i];
                    let fty = self.fast_ty(e, j);
                    let c = let1(
                        &mut o.f,
                        lets,
                        fty.clone(),
                        Op::Closure(id, Vec::new(), Vec::new()),
                        env.to_vec(),
                    );
                    let1(&mut o.f, lets, any(), Op::Box(fty), vec![v(c)])
                }
                None => let1(&mut o.f, lets, any(), Op::Box(Ty::I32), vec![int(0)]),
            };
            closures.push(v(b));
        }
        let closures = let1(
            &mut o.f,
            lets,
            Ty::Array(Box::new(any()), Mutability::Mut),
            Op::ArrayNew,
            closures,
        );
        let starts = let1(
            &mut o.f,
            lets,
            Ty::Array(Box::new(Ty::I32), Mutability::Imm),
            Op::Const(self.handlers[&h].starts),
            vec![],
        );
        let node = let1(
            &mut o.f,
            lets,
            self.rt.node_ty(),
            Op::New(self.rt.node()),
            vec![v(starts), v(mk), v(kinds), v(closures), v(ev)],
        );
        let start = let1(
            &mut o.f,
            lets,
            self.rt.start_ty(),
            Op::Closure(self.handlers[&h].start_body, Vec::new(), Vec::new()),
            body.to_vec(),
        );
        let mut args = vec![v(ev)];
        args.extend(env.iter().cloned());
        args.extend([v(node), v(start)]);
        lets.push(Let {
            binds: l.binds.clone(),
            op: Op::Call(self.handlers[&h].drive, Vec::new(), Vec::new()),
            args,
            source: l.source,
        });
    }
}
