//! Evidence passing: removes effect handlers from a module, so that any
//! backend can run it. See "Evidence passing" in `docs/lowerings.md`.
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
//! - **`throw`** is the one effect left, in modules where some function
//!   has it: every row of the lowered module gets it (see
//!   `throw_everywhere`).
//! - **Continuations** are closures that take a closure giving what the
//!   `perform` gives: `resume` passes one giving the values, and
//!   `discontinue` one that performs its operation there. A flag in each
//!   makes a second use trap. Resuming one runs the handler's `drive`
//!   again, under the handlers around the `resume`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use conduit_ir::build::{BlockRef, FuncBuilder, JoinPoint};
use conduit_ir::clause::{discontinues, plain_call};
use conduit_ir::{
    Atom, Block, ConstDecl, ConstId, ConstValue, CtorDecl, CtorId, DataDecl, DataId, EffectId,
    Func, FuncId, HandlerId, JoinDef, Label, Let, Lit, Module, MutParams, Mutability, Op, Row, Sig,
    Tail, Ty, Var,
};

/// Removes effect handlers from `m`. See the module's documentation.
pub fn lower_effects(m: &mut Module) -> Result<(), String> {
    if m.handlers.is_empty() && !uses_effects(m) {
        return Ok(());
    }
    let throws = may_throw(m);
    let mut cx = Lower::new(m)?;
    cx.prepare()?;
    cx.emit()?;
    if !cx.errors.is_empty() {
        return Err(cx.errors.join("\n"));
    }
    *m = cx.out;
    if throws {
        throw_everywhere(m);
    }
    Ok(())
}

/// Whether an exception from the host may unwind some of `m`'s code: some
/// function has `throw` in its row, or `Any`. One that calls an extern
/// with `throw` has it, so externs that nothing calls don't count.
fn may_throw(m: &Module) -> bool {
    m.funcs.values().any(|f| f.row.throws())
}

/// Gives every row in `m` `throw`. The lowering takes out every effect,
/// `throw` too; this puts it back where the backends look for it, in
/// modules that have it, without following where it went: every function
/// and closure may then throw, which costs a cleanup at more calls than
/// need one, but only in modules that both throw and are lowered.
fn throw_everywhere(m: &mut Module) {
    fn sig(s: &mut Sig) {
        s.row = s.row.with_throw(true);
        s.params.iter_mut().chain(&mut s.results).for_each(ty);
    }
    fn ty(t: &mut Ty) {
        match t {
            Ty::Fn(s, _) | Ty::Cont(s) => sig(s),
            Ty::Array(e, _) | Ty::Ref(e) => ty(e),
            _ => {}
        }
    }
    fn value(v: &mut ConstValue) {
        match v {
            ConstValue::Box(t, inner) => {
                ty(t);
                value(inner);
            }
            ConstValue::Array(parts) | ConstValue::New(_, parts) => {
                parts.iter_mut().for_each(value)
            }
            ConstValue::Lit(_) | ConstValue::Const(_) | ConstValue::Closure(_) => {}
        }
    }
    for f in m.funcs.values_mut() {
        f.row = f.row.with_throw(true);
        f.vars.iter_mut().chain(&mut f.results).for_each(ty);
        f.body.for_each_mut(|b| {
            for l in &mut b.lets {
                if let Op::Box(t) | Op::Unbox(t) = &mut l.op {
                    ty(t);
                }
            }
        });
    }
    for e in m.externs.values_mut() {
        sig(&mut e.sig);
    }
    for d in m.data.values_mut() {
        for c in &mut d.ctors {
            c.fields.iter_mut().for_each(ty);
        }
    }
    for c in m.consts.values_mut() {
        ty(&mut c.ty);
        value(&mut c.value);
    }
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
    /// `no_cont`, never called: what a `Yield` holds when it holds no
    /// continuation, as a constant closure of it.
    never: FuncId,
    /// `box[i32](0i32)` and a closure of `never`: what a `Yield` holds
    /// for its arguments and continuation when it holds none. Constants,
    /// so that nothing counts or frees them.
    no_args: ConstId,
    no_cont: ConstId,
}

impl Runtime {
    fn ev_ty(&self) -> Ty {
        Ty::data(self.ev_d, Mutability::Mut)
    }

    fn yield_ty(&self) -> Ty {
        Ty::data(self.yield_d, Mutability::Mut)
    }

    fn hs_ty(&self) -> Ty {
        Ty::data(self.hs_d, Mutability::Mut)
    }

    fn node_ty(&self) -> Ty {
        Ty::data(self.node_d, Mutability::Mut)
    }

    fn node(&self) -> CtorId {
        Runtime::ctor(self.node_d, 0)
    }

    /// A piece of a continuation: `fn(x, ev) -> any`, where `x` holds what
    /// the code it continues gave, and it gives what its function gives.
    /// `ro`, so that it can be `no_cont`, which is `imm`, as well as a
    /// closure the code makes, which is `mut`.
    fn piece_ty(&self) -> Ty {
        self.piece_ty_of(Mutability::ReadOnly)
    }

    fn piece_ty_of(&self, m: Mutability) -> Ty {
        Ty::Fn(
            Box::new(Sig::new(vec![any(), self.ev_ty()], vec![any()])),
            m,
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
    /// The adapter of `host_function` for each signature, by its
    /// parameters and results before lowering.
    host_adapters: HashMap<(Vec<Ty>, Vec<Ty>), FuncId>,
    /// The adapter of a `perform`'s continuation, by effect and operation.
    force: BTreeMap<(EffectId, u32), FuncId>,
    /// Tuples to box several values in, by their types.
    tuples: HashMap<(Vec<Ty>, u32), DataId>,
    /// The sites of each function, by the address of their `let`.
    sites: HashMap<*const Let, Site>,
    /// What builders check the functions of the output against, made
    /// before any is built: see [`Lower::declarations`].
    decls: Rc<Module>,
    /// Mistakes the builders found, which are lowering's.
    errors: Vec<String>,
}

// ---------------------------------------------------------------------
// Building IR.

fn vs(vars: &[Var]) -> Vec<Atom> {
    vars.iter().map(|&x| Atom::Var(x)).collect()
}

/// The number after every ID of `items`, which are ordered by ID.
fn next_id<K: Copy, V>(items: &BTreeMap<K, V>, number: impl Fn(K) -> u64) -> u64 {
    items.keys().next_back().map_or(0, |&k| number(k) + 1)
}

/// The mutability parameters of a function as arguments to another with
/// the same ones.
fn same_margs(f: &Func) -> Vec<Mutability> {
    (1..=f.mut_params.count).map(Mutability::Param).collect()
}

fn any_let(b: &Block, test: &mut impl FnMut(&Let) -> bool) -> bool {
    b.all_lets().any(test)
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
            used.extend(l.args.iter().filter_map(Atom::var));
            bound.extend(l.binds.iter().copied());
        }
        used.extend(tail.atoms().iter().filter_map(Atom::var));
        if let Tail::Join { defs, .. } = tail {
            bound.extend(defs.iter().flat_map(|d| d.params.iter().copied()));
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
    for b in children.into_iter().flat_map(Block::blocks) {
        visit(&b.lets, &b.tail);
    }
    used.difference(&bound).copied().collect()
}

/// The labels `b` jumps to.
fn jumped_labels(b: &Block) -> BTreeSet<Label> {
    (b.blocks())
        .filter_map(|b| match &b.tail {
            Tail::Jump { target, .. } => Some(*target),
            _ => None,
        })
        .collect()
}

/// The labels `b` defines.
fn defined_labels(b: &Block) -> BTreeSet<Label> {
    b.defs().map(|d| d.label).collect()
}

// ---------------------------------------------------------------------
// Setting up.

impl Lower {
    fn new(m: &Module) -> Result<Lower, String> {
        let next_func = next_id(&m.funcs, |f| f.0);
        let next_data = next_id(&m.data, |d| d.0);
        let next_const = next_id(&m.consts, |c| c.0);
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
                never: FuncId(next_func + 3),
                no_args: ConstId(next_const),
                no_cont: ConstId(next_const + 1),
            },
            next_func: next_func + 4,
            next_data: next_data + 4,
            yields: BTreeSet::new(),
            takes_ev: BTreeSet::new(),
            internal: BTreeMap::new(),
            ev_wrappers: BTreeMap::new(),
            handlers: BTreeMap::new(),
            perf: HashMap::new(),
            consts: HashMap::new(),
            host_adapters: HashMap::new(),
            force: BTreeMap::new(),
            tuples: HashMap::new(),
            sites: HashMap::new(),
            decls: Rc::default(),
            errors: Vec::new(),
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
        let mut never = Func::new(vec![any(), rt.ev_ty()], vec![any()]);
        never.debug.name = Some("no_cont".into());
        self.out.funcs.insert(rt.never, never);
        self.out.consts.insert(
            rt.no_args,
            ConstDecl {
                ty: Ty::Any(Mutability::Imm),
                value: ConstValue::Box(Ty::I32, Box::new(ConstValue::Lit(Lit::I32(0)))),
                debug_name: Some("no_args".into()),
            },
        );
        self.out.consts.insert(
            rt.no_cont,
            ConstDecl {
                ty: rt.piece_ty_of(Mutability::Imm),
                value: ConstValue::Closure(rt.never),
                debug_name: Some("no_cont".into()),
            },
        );
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
        let next = next_id(&self.src.consts, |c| c.0).max(next_id(&self.out.consts, |c| c.0));
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
            for l in f.body.all_lets() {
                if let (Op::Discontinue(e, i), Some(Atom::Var(k))) = (&l.op, l.args.first())
                    && let Some(Ty::Cont(sig)) = f.vars.get(k.0 as usize)
                {
                    let key = (*e, *i, sig.params.clone());
                    if !discontinued.contains(&key) {
                        discontinued.push(key);
                    }
                }
            }
        }
        for (e, i, takes) in discontinued {
            let params = self.src.effects[&e].ops[i as usize].params.clone();
            let mut b = FuncBuilder::new(&self.src, params, takes.clone());
            b.set_name(format!("discontinue_{}", e.0));
            b.set_row(Row::one(e));
            // What the operation gives fits what the continuation takes.
            let args = b.param_vars().to_vec();
            let rs = b.perform(e, i, args);
            b.ret(rs);
            let f = b.finish().map_err(|errors| {
                let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
                format!("discontinue_{}: {}", e.0, errors.join("; "))
            })?;
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
        self.may_yield(&b.lets[i], vars) && !is_tail(b, i)
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
                Tail::Case { .. } | Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {
                    for c in b.children() {
                        self.scope_at_sites(c, scope, vars, lifted, defs);
                    }
                }
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
                    let rs: Vec<Var> = (orig.results.iter())
                        .map(|t| into.new_var(t.clone()))
                        .collect();
                    lets.push(Let {
                        binds: rs.clone(),
                        op: Op::Call(lifted.func, same_margs(orig), rows),
                        args: all,
                        source,
                    });
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
                    return Block {
                        lets,
                        tail: body.tail,
                    };
                }
                Tail::Join {
                    rec,
                    defs,
                    body: Box::new(body),
                }
            }
            t @ (Tail::Case { .. } | Tail::Unreachable | Tail::Trap) => {
                t.map_blocks(|b| rewrite_jumps(b, table, into, orig))
            }
        };
        Block { lets, tail }
    })
}

// ---------------------------------------------------------------------
// Types and the runtime.

/// What builds the functions of the output. It checks what it builds
/// against the declarations of every function and data type of the output
/// that [`Lower::declarations`] knows of before any is built, and against
/// what's declared to it as it goes: the tuples, rests and adapters that
/// lowering makes as it finds that it needs them.
type B<'d> = FuncBuilder<'d, Module>;

/// A builder of a function of signature `sig`, named `name`.
fn builder<'d>(decls: &'d Module, sig: &Sig, name: impl Into<Box<str>>) -> B<'d> {
    let mut b = FuncBuilder::new(decls, sig.params.clone(), sig.results.clone());
    b.set_name(name);
    b
}

/// Two blocks for `case x { 0 => .., _ => .. }`: the zero arm, then the
/// default.
fn case_zero(b: &mut B<'_>, x: Var) -> (BlockRef, BlockRef) {
    let case = b.case(x);
    let zero = b.arm(case, 0i32);
    let other = b.default(case);
    (zero, other)
}

/// `value`, of type `ty`, boxed as an `ro any`, which holds any value.
fn box_any(b: &mut B<'_>, ty: Ty, value: impl Into<Atom>) -> Var {
    b.op_as(Op::Box(ty), [value.into()], vec![any()])[0]
}

/// Forgets those of `vars` that are objects: what something that yielded
/// gave, which is undefined, so that nothing drops it.
fn forget(b: &mut B<'_>, vars: &[Var]) {
    for &x in vars {
        if b.var_ty(x).is_some_and(Ty::is_object) {
            b.op(Op::Forget, [x]);
        }
    }
}

/// `undefined` values of types `tys`.
fn undefineds(b: &mut B<'_>, tys: &[Ty]) -> Vec<Var> {
    tys.iter().map(|t| b.undefined(t.clone())).collect()
}

/// `ret` with an undefined `ro any`.
fn ret_undefined_any(b: &mut B<'_>) {
    let u = b.undefined(any());
    b.ret([u]);
}

/// A placeholder of a function with this signature and these mutability
/// parameters, which declares it.
fn declared(sig: Sig, mut_params: MutParams) -> Func {
    let mut f = Func::new(sig.params, sig.results);
    f.mut_params = mut_params;
    f
}

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

    /// The signature of a function of the output that takes `params`, then
    /// the evidence.
    fn ev_sig(&self, mut params: Vec<Ty>, results: Vec<Ty>) -> Sig {
        params.push(self.rt.ev_ty());
        Sig::new(params, results)
    }

    /// The signature of `src` after lowering, without evidence.
    fn lowered_sig(&self, src: &Func) -> Sig {
        Sig::new(self.tys(&src.sig().params), self.tys(&src.results))
    }

    /// The signatures of the runtime's functions.
    fn runtime_sigs(&self) -> [(FuncId, Sig); 3] {
        let (ev, piece) = (self.rt.ev_ty(), self.rt.piece_ty());
        [
            (
                self.rt.lookup,
                Sig::new(vec![ev.clone(), Ty::I32], vec![self.rt.node_ty(), Ty::I32]),
            ),
            (
                self.rt.compose,
                Sig::new(
                    vec![piece.clone(), piece.clone(), any(), ev.clone()],
                    vec![any()],
                ),
            ),
            (
                self.rt.start_resume,
                Sig::new(vec![piece, any(), ev], vec![any()]),
            ),
        ]
    }

    /// Every function of the output that exists before any is built, as
    /// placeholders that only declare them, and every data type and
    /// constant: what each builder checks its function against, so that
    /// functions can call each other however they're ordered.
    fn declarations(&self) -> Module {
        let mut m = Module {
            data: self.out.data.clone(),
            externs: self.out.externs.clone(),
            consts: self.out.consts.clone(),
            ..Module::default()
        };
        for (&id, f) in &self.out.funcs {
            m.funcs.insert(id, f.without_body());
        }
        for (id, sig) in self.runtime_sigs() {
            m.funcs.insert(id, declared(sig, MutParams::default()));
        }
        for (h, funcs) in &self.handlers {
            let ids = [funcs.start_body, funcs.drive, funcs.reprompt, funcs.kont];
            for (id, sig) in ids.into_iter().zip(self.handler_sigs(*h)) {
                m.funcs.insert(id, declared(sig, MutParams::default()));
            }
        }
        for (&id, src) in &self.src.funcs {
            let sig = self.lowered_sig(src);
            if let Some(&internal) = self.internal.get(&id) {
                let with_ev = self.ev_sig(sig.params.clone(), sig.results.clone());
                m.funcs
                    .insert(internal, declared(with_ev, src.mut_params.clone()));
            }
            m.funcs.insert(id, declared(sig, src.mut_params.clone()));
        }
        m
    }

    /// The function `b` built, as `id` of the output. A mistake in it is
    /// lowering's, kept to report.
    fn finish(&mut self, id: FuncId, b: B<'_>) {
        match b.finish() {
            Ok(f) => {
                self.out.funcs.insert(id, f);
            }
            Err(errors) => {
                let errors = errors.into_iter().map(|e| format!("lowering to {id}: {e}"));
                self.errors.extend(errors);
            }
        }
    }

    /// Tells `b` about function `f` of the output, made while lowering, if
    /// it doesn't know it yet.
    fn declare(&self, b: &mut B<'_>, f: FuncId) {
        if let Some(func) = self.out.funcs.get(&f)
            && !b.knows_func(f)
        {
            b.declare_func(f, func.sig(), func.mut_params.clone(), 0);
        }
    }

    /// The tuple that boxes values of types `tys`, in a function with
    /// `params` mutability parameters, whose types they may use, which `b`
    /// is then told about.
    fn tuple(&mut self, b: &mut B<'_>, tys: &[Ty], params: u32) -> DataId {
        let key = (tys.to_vec(), params);
        let d = match self.tuples.get(&key) {
            Some(&d) => d,
            None => {
                let d = self.fresh_data();
                let decl = DataDecl {
                    params,
                    effect_params: 0,
                    ctors: vec![CtorDecl {
                        fields: tys.to_vec(),
                    }],
                    debug_name: Some(format!("Tuple{}", tys.len()).into()),
                };
                self.out.data.insert(d, decl);
                self.tuples.insert(key, d);
                d
            }
        };
        if !b.knows_data(d) {
            b.declare_data(d, self.out.data[&d].clone());
        }
        d
    }

    /// Boxes `vals` of types `tys` into one `ro any`: none as a box of 0,
    /// one as itself, several in a tuple, in a function whose mutability
    /// parameters are `margs`.
    fn box_vals(
        &mut self,
        b: &mut B<'_>,
        margs: &[Mutability],
        vals: Vec<Atom>,
        tys: &[Ty],
    ) -> Var {
        let (ty, val) = match vals.len() {
            0 => (Ty::I32, Atom::from(0i32)),
            1 => (tys[0].clone(), vals[0].clone()),
            _ => {
                let d = self.tuple(b, tys, margs.len() as u32);
                let t = Ty::Data(d, Mutability::Mut, margs.to_vec(), Vec::new());
                let ctor = Runtime::ctor(d, 0);
                let x = b.new_object_with(ctor, Mutability::Mut, margs.to_vec(), vals);
                (t, Atom::Var(x))
            }
        };
        box_any(b, ty, val)
    }

    /// The values of types `tys` that [`box_vals`](Self::box_vals) boxed
    /// in `x`.
    fn unbox_vals(&mut self, b: &mut B<'_>, margs: &[Mutability], x: Var, tys: &[Ty]) -> Vec<Var> {
        match tys {
            [] => Vec::new(),
            [t] => vec![b.unbox(t.clone(), x)],
            _ => {
                let d = self.tuple(b, tys, margs.len() as u32);
                let t = Ty::Data(d, Mutability::Mut, margs.to_vec(), Vec::new());
                let tup = b.unbox(t, x);
                b.fields(Runtime::ctor(d, 0), tup)
            }
        }
    }

    /// The shared state of evidence `ev`: its `Yield`.
    fn yield_state(&self, b: &mut B<'_>, ev: Var) -> Var {
        b.get(Runtime::ctor(self.rt.ev_d, 0), 0, ev)
    }

    /// Whether the code is yielding: an `i32`.
    fn yielding(&self, b: &mut B<'_>, ev: Var) -> Var {
        let st = self.yield_state(b, ev);
        let marker = b.get(Runtime::ctor(self.rt.yield_d, 0), 0, st);
        b.ne(marker, 0i32)
    }

    /// Adds `piece` to the continuation being built.
    fn extend(&self, b: &mut B<'_>, ev: Var, piece: Var) {
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let st = self.yield_state(b, ev);
        let cur = b.get(yc, 3, st);
        let c = b.closure(self.rt.compose, Mutability::Mut, [cur, piece]);
        b.set(yc, 3, st, c);
    }

    /// What a `Yield` holds for its arguments when it holds none.
    fn no_args(&self, b: &mut B<'_>) -> Var {
        b.constant(self.rt.no_args)
    }

    /// What a `Yield` holds for its continuation when it holds none.
    fn no_cont(&self, b: &mut B<'_>) -> Var {
        b.constant(self.rt.no_cont)
    }

    /// The runtime's own functions.
    fn runtime_funcs(&mut self) {
        let decls = Rc::clone(&self.decls);
        let [
            (lookup, lookup_sig),
            (compose, compose_sig),
            (start_resume, resume_sig),
        ] = self.runtime_sigs();
        let hs_c = Runtime::ctor(self.rt.hs_d, 1);
        let ev_c = Runtime::ctor(self.rt.ev_d, 0);
        let node_c = self.rt.node();

        // lookup(ev, effect) -> (node, start), walking the handlers from
        // the innermost.
        let mut b = builder(&decls, &lookup_sig, "lookup");
        let [ev, e] = b.params();
        let first = b.get(ev_c, 1, ev);
        let lp = b.join_rec(&[self.rt.hs_ty()]);
        b.jump(&lp, [first]);
        b.switch_to(lp.block());
        let hs = lp.param(0);
        let case = b.case(hs);
        // No handler: an unhandled effect stops the program.
        b.arm(case, Runtime::ctor(self.rt.hs_d, 0));
        b.trap();
        b.arm(case, hs_c);
        let node = b.get(hs_c, 0, hs);
        let starts = b.get(node_c, 0, node);
        let start = b.array_get(starts, e);
        let handles = b.ge_s(start, 0i32);
        let (outer, found) = case_zero(&mut b, handles);
        b.switch_to(outer);
        let outer_ev = b.get(node_c, 4, node);
        let next = b.get(ev_c, 1, outer_ev);
        b.jump(&lp, [next]);
        b.switch_to(found);
        b.ret([node, start]);
        self.finish(lookup, b);

        // compose(prev, next, x, ev) -> any
        let mut b = builder(&decls, &compose_sig, "compose");
        let [prev, next, x, ev] = b.params();
        let y = b.apply1(prev, [x, ev]);
        let yielding = self.yielding(&mut b, ev);
        let (go, stop) = case_zero(&mut b, yielding);
        b.switch_to(go);
        let r = b.apply1(next, [y, ev]);
        b.ret([r]);
        b.switch_to(stop);
        forget(&mut b, &[y]);
        self.extend(&mut b, ev, next);
        ret_undefined_any(&mut b);
        self.finish(compose, b);

        // start_resume(cont, x, ev) -> any
        let mut b = builder(&decls, &resume_sig, "start_resume");
        let [cont, x, ev] = b.params();
        let r = b.apply1(cont, [x, ev]);
        b.ret([r]);
        self.finish(start_resume, b);
    }

    /// `const(vals.., ev) -> vals`, by the types of the values, which `b`
    /// is then told about.
    fn const_func(&mut self, b: &mut B<'_>, tys: &[Ty]) -> FuncId {
        let id = match self.consts.get(tys) {
            Some(&id) => id,
            None => {
                let id = self.fresh_func();
                let decls = Rc::clone(&self.decls);
                let sig = self.ev_sig(tys.to_vec(), tys.to_vec());
                let mut f = builder(&decls, &sig, "resume_with");
                let vals = f.param_vars()[..tys.len()].to_vec();
                f.ret(vals);
                self.finish(id, f);
                self.consts.insert(tys.to_vec(), id);
                id
            }
        };
        self.declare(b, id);
        id
    }

    /// `force_E_i(next, site_ev, x, ev) -> any`: the first piece of the
    /// continuation of a `perform E.i`. It calls the closure in `x`, which
    /// gives what the `perform` gives, at the `perform`, then `next`, the
    /// rest of the `perform`'s function. `b` is then told about it.
    fn force_func(&mut self, b: &mut B<'_>, e: EffectId, i: u32) -> FuncId {
        let id = match self.force.get(&(e, i)) {
            Some(&id) => id,
            None => {
                let id = self.fresh_func();
                self.make_force(id, e, i);
                self.force.insert((e, i), id);
                id
            }
        };
        self.declare(b, id);
        id
    }

    fn make_force(&mut self, id: FuncId, e: EffectId, i: u32) {
        let decls = Rc::clone(&self.decls);
        let results = self.tys(&self.src.effects[&e].ops[i as usize].results);
        let ev_ty = self.rt.ev_ty();
        let sig = Sig::new(
            vec![self.rt.piece_ty(), ev_ty.clone(), any(), ev_ty],
            vec![any()],
        );
        let mut b = builder(&decls, &sig, "force");
        let [next, site_ev, x, _] = b.params();
        let t = b.unbox(self.thunk_ty(&results), x);
        let xs = b.apply(t, [site_ev]);
        let yielding = self.yielding(&mut b, site_ev);
        let (go, stop) = case_zero(&mut b, yielding);
        b.switch_to(go);
        let bx = self.box_vals(&mut b, &[], vs(&xs), &results);
        let r = b.apply1(next, [bx, site_ev]);
        b.ret([r]);
        b.switch_to(stop);
        forget(&mut b, &xs);
        self.extend(&mut b, site_ev, next);
        ret_undefined_any(&mut b);
        self.finish(id, b);
    }

    /// The function a closure of `g` calls: `g` itself if it takes
    /// evidence, or one that takes it and calls `g`. `b` is then told
    /// about it.
    fn ev_version(&mut self, b: &mut B<'_>, g: FuncId) -> FuncId {
        if let Some(&id) = self.internal.get(&g) {
            return id;
        }
        let id = match self.ev_wrappers.get(&g) {
            Some(&id) => id,
            None => {
                let id = self.fresh_func();
                self.make_ev_version(id, g);
                self.ev_wrappers.insert(g, id);
                id
            }
        };
        self.declare(b, id);
        id
    }

    /// `v` with each closure of a function made one of its version that
    /// takes evidence.
    fn const_closures(&mut self, v: &ConstValue) -> ConstValue {
        match v {
            ConstValue::Closure(g) => {
                let id = match (self.internal.get(g), self.ev_wrappers.get(g)) {
                    (Some(&id), _) | (None, Some(&id)) => id,
                    (None, None) => {
                        let id = self.fresh_func();
                        self.make_ev_version(id, *g);
                        self.ev_wrappers.insert(*g, id);
                        id
                    }
                };
                ConstValue::Closure(id)
            }
            ConstValue::Array(xs) => {
                ConstValue::Array(xs.iter().map(|x| self.const_closures(x)).collect())
            }
            ConstValue::New(c, xs) => {
                ConstValue::New(*c, xs.iter().map(|x| self.const_closures(x)).collect())
            }
            ConstValue::Box(ty, x) => {
                ConstValue::Box(self.ty(ty), Box::new(self.const_closures(x)))
            }
            ConstValue::Lit(_) | ConstValue::Const(_) => v.clone(),
        }
    }

    fn make_ev_version(&mut self, id: FuncId, g: FuncId) {
        let decls = Rc::clone(&self.decls);
        let src = self.src.funcs[&g].without_body();
        let sig = self.lowered_sig(&src);
        let n = sig.params.len();
        let name = src
            .debug
            .name
            .as_deref()
            .map_or(g.to_string(), str::to_string);
        let ev_sig = self.ev_sig(sig.params, sig.results);
        let mut b = builder(&decls, &ev_sig, format!("{name}_ev"));
        b.set_mut_params(src.mut_params.clone());
        let args = b.param_vars()[..n].to_vec();
        let rs = b.call_at(g, same_margs(&src), args);
        b.ret(rs);
        self.finish(id, b);
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

    /// The signatures of the functions `h` is lowered to: `start_body`,
    /// `drive`, `reprompt` and `kont`.
    fn handler_sigs(&self, h: HandlerId) -> [Sig; 4] {
        let (results, body_results, env) = self.handler_types(h);
        let (ev, node, piece) = (self.rt.ev_ty(), self.rt.node_ty(), self.rt.piece_ty());
        let body_ty = Ty::Fn(
            Box::new(Sig::new(vec![ev.clone()], body_results)),
            Mutability::ReadOnly,
        );
        let start_body = Sig::new(vec![body_ty, ev.clone()], vec![any()]);
        let mut drive = vec![ev.clone()];
        drive.extend(env.iter().cloned());
        drive.extend([node.clone(), self.rt.start_ty()]);
        let mut reprompt = env.clone();
        reprompt.extend([node.clone(), piece.clone(), any(), ev.clone()]);
        let mut kont = env;
        kont.extend([node, piece, Ty::Ref(Box::new(Ty::I32)), any(), ev]);
        [
            start_body,
            Sig::new(drive, results.clone()),
            Sig::new(reprompt, vec![any()]),
            Sig::new(kont, results),
        ]
    }

    /// Calls clause or return clause `clause` with `args`, passing `ev` if
    /// it takes evidence.
    fn clause_call(&self, b: &mut B<'_>, clause: FuncId, mut args: Vec<Atom>, ev: Var) -> Vec<Var> {
        let id = match self.internal.get(&clause) {
            Some(&id) => {
                args.push(ev.into());
                id
            }
            None => clause,
        };
        b.call(id, args)
    }

    fn handler_funcs(&mut self, h: HandlerId) {
        let decls = Rc::clone(&self.decls);
        let (results, body_results, env_tys) = self.handler_types(h);
        let [start_body_sig, drive_sig, reprompt_sig, kont_sig] = self.handler_sigs(h);
        let decl = self.src.handlers[&h].clone();
        let funcs = &self.handlers[&h];
        let (start_body, drive, reprompt, kont) =
            (funcs.start_body, funcs.drive, funcs.reprompt, funcs.kont);
        let fast = funcs.fast.clone();
        let name = decl
            .debug_name
            .as_deref()
            .map_or(h.to_string(), str::to_string);
        let node_c = self.rt.node();
        let ev_c = Runtime::ctor(self.rt.ev_d, 0);
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let n_env = env_tys.len();

        // start_body(body, ev) -> any
        let mut b = builder(&decls, &start_body_sig, format!("{name}_start"));
        let [body, ev] = b.params();
        let rs = b.apply(body, [ev]);
        let yielding = self.yielding(&mut b, ev);
        let (go, stop) = case_zero(&mut b, yielding);
        b.switch_to(go);
        let bx = self.box_vals(&mut b, &[], vs(&rs), &body_results);
        b.ret([bx]);
        b.switch_to(stop);
        forget(&mut b, &rs);
        ret_undefined_any(&mut b);
        self.finish(start_body, b);

        // drive(ev, env.., node, start) -> R
        let mut b = builder(&decls, &drive_sig, format!("{name}_drive"));
        let params = b.param_vars().to_vec();
        let ev_outer = params[0];
        let env = params[1..1 + n_env].to_vec();
        let (node, start) = (params[1 + n_env], params[2 + n_env]);
        b.set(node_c, 4, node, ev_outer);
        let st = self.yield_state(&mut b, ev_outer);
        let hs = b.new_object(Runtime::ctor(self.rt.hs_d, 1), Mutability::Mut, [node]);
        let ev_in = b.new_object(ev_c, Mutability::Mut, [st, hs]);
        let r = b.apply1(start, [ev_in]);
        let marker = b.get(yc, 0, st);
        let (done, yielding) = case_zero(&mut b, marker);
        // The body returned.
        b.switch_to(done);
        let bs = self.unbox_vals(&mut b, &[], r, &body_results);
        let args = vs(&env).into_iter().chain(vs(&bs)).collect();
        let out = self.clause_call(&mut b, decl.ret, args, ev_outer);
        b.ret(out);
        // Yielding: to another handler, or to this one.
        b.switch_to(yielding);
        forget(&mut b, &[r]);
        let my_marker = b.get(node_c, 1, node);
        let same = b.eq(marker, my_marker);
        let (other, mine) = case_zero(&mut b, same);
        // To another: resuming the continuation must run under this one
        // again.
        b.switch_to(other);
        let inner = b.get(yc, 3, st);
        let caps: Vec<Var> = env.iter().copied().chain([node, inner]).collect();
        let w = b.closure(reprompt, Mutability::Mut, caps);
        b.set(yc, 3, st, w);
        let us = undefineds(&mut b, &results);
        b.ret(us);
        // To this one: run the clause with the continuation.
        b.switch_to(mine);
        let op_n = b.get(yc, 1, st);
        let args_any = b.get(yc, 2, st);
        let cont = b.get(yc, 3, st);
        b.set(yc, 0, st, 0i32);
        // The state lets go of what it held for this yield. Otherwise it
        // keeps the continuation, whose closures hold evidence that points
        // back at the state: a cycle, which reference counting can't free,
        // and which keeps everything the continuation holds alive.
        let no_args = self.no_args(&mut b);
        b.set(yc, 2, st, no_args);
        let no_cont = self.no_cont(&mut b);
        b.set(yc, 3, st, no_cont);
        let used = b.ref_new(0i32);
        let caps: Vec<Var> = env.iter().copied().chain([node, cont, used]).collect();
        let k = b.closure(kont, Mutability::Mut, caps);
        let case = b.case(op_n);
        for (i, (e, j, clause)) in decl.clauses().enumerate() {
            if fast[i].is_some() {
                continue;
            }
            b.arm(case, i as i64);
            let ptys = self.tys(&self.src.effects[&e].ops[j as usize].params);
            let ps = self.unbox_vals(&mut b, &[], args_any, &ptys);
            let args = (vs(&env).into_iter())
                .chain(vs(&ps))
                .chain([Atom::Var(k)])
                .collect();
            let r = self.clause_call(&mut b, clause, args, ev_outer);
            b.ret(r);
        }
        b.default(case);
        b.unreachable();
        self.finish(drive, b);

        // reprompt(env.., node, inner, x, ev) -> any
        let mut b = builder(&decls, &reprompt_sig, format!("{name}_reprompt"));
        let params = b.param_vars().to_vec();
        let env = params[..n_env].to_vec();
        let [node, inner, x, ev] = params[n_env..].try_into().unwrap();
        let s = b.closure(self.rt.start_resume, Mutability::Mut, [inner, x]);
        let args: Vec<Var> = [ev].into_iter().chain(env).chain([node, s]).collect();
        let rs = b.call(drive, args);
        let yielding = self.yielding(&mut b, ev);
        let (go, stop) = case_zero(&mut b, yielding);
        b.switch_to(go);
        let bx = self.box_vals(&mut b, &[], vs(&rs), &results);
        b.ret([bx]);
        b.switch_to(stop);
        forget(&mut b, &rs);
        ret_undefined_any(&mut b);
        self.finish(reprompt, b);

        // kont(env.., node, cont, used, x, ev) -> R
        let mut b = builder(&decls, &kont_sig, format!("{name}_k"));
        let params = b.param_vars().to_vec();
        let env = params[..n_env].to_vec();
        let [node, cont, used, x, ev] = params[n_env..].try_into().unwrap();
        let u = b.ref_get(used);
        let (first, again) = case_zero(&mut b, u);
        b.switch_to(first);
        b.ref_set(used, 1i32);
        let s = b.closure(self.rt.start_resume, Mutability::Mut, [cont, x]);
        let args: Vec<Var> = [ev].into_iter().chain(env).chain([node, s]).collect();
        let rs = b.call(drive, args);
        b.ret(rs);
        // A second use traps.
        b.switch_to(again);
        b.trap();
        self.finish(kont, b);
    }
}

// ---------------------------------------------------------------------
// Functions.

/// A function being lowered.
struct Out<'s, 'd> {
    /// The function it's from. A rest is from the function of its site.
    src: &'s Func,
    b: B<'d>,
    /// The variable holding the evidence, if it takes some.
    ev: Option<Var>,
    /// Whether it's a rest, which gives what its function would, boxed.
    boxed: bool,
    /// The mutability parameters of `src`, which it has too, as arguments.
    margs: Vec<Mutability>,
    /// What each variable and join point of `src` is in it.
    vars: HashMap<Var, Var>,
    labels: HashMap<Label, JoinPoint>,
}

impl<'s, 'd> Out<'s, 'd> {
    fn new(src: &'s Func, mut b: B<'d>, boxed: bool) -> Out<'s, 'd> {
        b.set_mut_params(src.mut_params.clone());
        Out {
            src,
            b,
            ev: None,
            boxed,
            margs: same_margs(src),
            vars: HashMap::new(),
            labels: HashMap::new(),
        }
    }

    fn var(&self, v: Var) -> Var {
        self.vars[&v]
    }

    fn vars(&self, vars: &[Var]) -> Vec<Var> {
        vars.iter().map(|&v| self.var(v)).collect()
    }

    fn atom(&self, a: &Atom) -> Atom {
        match a {
            Atom::Var(v) => Atom::Var(self.var(*v)),
            lit => lit.clone(),
        }
    }

    fn atoms(&self, atoms: &[Atom]) -> Vec<Atom> {
        atoms.iter().map(|a| self.atom(a)).collect()
    }

    /// Makes `outs` what the variables `src_vars` of `src` are, named as
    /// they are.
    fn bind(&mut self, src_vars: &[Var], outs: &[Var]) {
        for (&s, &o) in src_vars.iter().zip(outs) {
            self.vars.insert(s, o);
            if let Some(name) = self.src.debug.vars.get(&s) {
                self.b.name_var(o, name.clone());
            }
        }
    }
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
        self.decls = Rc::new(self.declarations());
        self.runtime_funcs();
        // A constant closure's type takes evidence now, so its function
        // must too, as for a `closure` op.
        let consts: Vec<ConstId> = self.src.consts.keys().copied().collect();
        for c in consts {
            let value = self.out.consts[&c].value.clone();
            let value = self.const_closures(&value);
            self.out.consts.get_mut(&c).unwrap().value = value;
        }
        for h in self.src.handlers.keys().copied().collect::<Vec<_>>() {
            self.handler_funcs(h);
        }
        let ids: Vec<FuncId> = self.src.funcs.keys().copied().collect();
        for id in ids {
            self.lower_func(id);
        }
        Ok(())
    }

    /// Lowers function `id` of the source, as the function that takes
    /// evidence and its wrapper at `id` if it takes some.
    fn lower_func(&mut self, id: FuncId) {
        let src = Rc::new(self.src.funcs[&id].clone());
        let decls = Rc::clone(&self.decls);
        let internal = self.internal.get(&id).copied();
        let mut sig = self.lowered_sig(&src);
        if internal.is_some() {
            sig = self.ev_sig(sig.params, sig.results);
        }
        let name = match (&src.debug.name, internal) {
            (Some(name), Some(_)) => Some(format!("{name}_ev")),
            (name, _) => name.as_deref().map(str::to_string),
        };
        let mut b = FuncBuilder::new(&*decls, sig.params, sig.results);
        if let Some(name) = name {
            b.set_name(name);
        }
        let mut o = Out::new(&src, b, false);
        let params = o.b.param_vars().to_vec();
        o.bind(&src.params, &params);
        o.ev = internal.map(|_| *params.last().unwrap());
        self.block_from(&mut o, &src.body, 0);
        // Sites are found by the address of their `let`, which another
        // function's could have once this one is dropped.
        self.sites.clear();
        match internal {
            Some(internal) => {
                self.finish(internal, o.b);
                self.entry_wrapper(id, &src, internal);
            }
            None => self.finish(id, o.b),
        }
    }

    /// The function at the old ID of `src`, which takes evidence as
    /// `internal`: it makes new evidence and calls `internal`. A yield
    /// that gets this far has no handler, which can't happen, since
    /// `perform` traps first.
    fn entry_wrapper(&mut self, id: FuncId, src: &Func, internal: FuncId) {
        let decls = Rc::clone(&self.decls);
        let sig = self.lowered_sig(src);
        let mut b = FuncBuilder::new(&*decls, sig.params, sig.results);
        if let Some(name) = &src.debug.name {
            b.set_name(name.clone());
        }
        b.set_mut_params(src.mut_params.clone());
        let (ev, st) = self.fresh_ev(&mut b);
        let mut args = vs(b.param_vars());
        args.push(ev.into());
        let rs = b.call_at(internal, same_margs(src), args);
        let marker = b.get(Runtime::ctor(self.rt.yield_d, 0), 0, st);
        let (done, yielding) = case_zero(&mut b, marker);
        b.switch_to(done);
        b.ret(rs);
        b.switch_to(yielding);
        b.unreachable();
        self.finish(id, b);
    }

    /// Evidence for code the host calls: no handlers, and nothing
    /// yielding. Gives the evidence and its yield state, whose marker is 0
    /// unless the code performed an operation that nothing handles.
    fn fresh_ev(&self, b: &mut B<'_>) -> (Var, Var) {
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let zero = b.boxed(Ty::I32, 0i32);
        let no_cont = self.no_cont(b);
        let fields: [Atom; 5] = [
            0i32.into(),
            0i32.into(),
            zero.into(),
            no_cont.into(),
            1i32.into(),
        ];
        let st = b.new_object(yc, Mutability::Mut, fields);
        let hs = b.new_object(Runtime::ctor(self.rt.hs_d, 0), Mutability::Mut, ());
        let ev = b.new_object(Runtime::ctor(self.rt.ev_d, 0), Mutability::Mut, [st, hs]);
        (ev, st)
    }

    /// The function behind `host_function` of a closure of signature `sig`
    /// before lowering: it takes the lowered closure, which takes evidence,
    /// then the host's arguments, and applies the closure with fresh
    /// evidence, as an export's wrapper calls its function. So the closure
    /// the host gets takes only what the host gives. `b` is then told about
    /// it.
    fn host_adapter(&mut self, b: &mut B<'_>, sig: &Sig) -> FuncId {
        let key = (sig.params.clone(), sig.results.clone());
        let id = match self.host_adapters.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.fresh_func();
                self.make_host_adapter(id, sig);
                self.host_adapters.insert(key, id);
                id
            }
        };
        self.declare(b, id);
        id
    }

    fn make_host_adapter(&mut self, id: FuncId, sig: &Sig) {
        let decls = Rc::clone(&self.decls);
        // `ro`, so that it takes the closure whatever its mutability.
        let closure = self.ty(&Ty::Fn(Box::new(sig.clone()), Mutability::ReadOnly));
        let mut params = vec![closure];
        params.extend(self.tys(&sig.params));
        let adapter = Sig::new(params, self.tys(&sig.results));
        let mut b = builder(&decls, &adapter, "host_adapter");
        let (ev, st) = self.fresh_ev(&mut b);
        let (c, args) = b.param_vars().split_first().unwrap();
        let (c, mut args) = (*c, vs(args));
        args.push(ev.into());
        let rs = b.apply(c, args);
        let marker = b.get(Runtime::ctor(self.rt.yield_d, 0), 0, st);
        let (done, yielding) = case_zero(&mut b, marker);
        b.switch_to(done);
        b.ret(rs);
        b.switch_to(yielding);
        b.unreachable();
        self.finish(id, b);
    }

    /// Lowers `lets[start..]` and the tail of `b`, which ends the block
    /// being built.
    fn block_from(&mut self, o: &mut Out<'_, '_>, b: &Block, start: usize) {
        conduit_ir::deep(|| {
            for i in start..b.lets.len() {
                let l = &b.lets[i];
                o.b.set_source(l.source);
                if let Op::Perform(e, op) = l.op
                    && self.yields.contains(&e)
                {
                    return self.perform_site(o, b, i, e, op);
                }
                self.lower_let(o, l);
                if let Some(ev) = o.ev
                    && self.is_site(b, i, &o.src.vars)
                {
                    let site = self.site(o, b, i);
                    let yielding = self.yielding(&mut o.b, ev);
                    let (go, stop) = case_zero(&mut o.b, yielding);
                    o.b.switch_to(go);
                    self.continue_after(o, b, i, &site);
                    o.b.switch_to(stop);
                    return self.yield_block(o, &site);
                }
            }
            // A rest boxes what a tail call gives. If the call yielded,
            // that's undefined, and so the rest gives an undefined box
            // too, as everything that yields does.
            if let (true, Some(ev), Some(l)) = (o.boxed, o.ev, b.lets.last())
                && is_tail(b, b.lets.len() - 1)
                && self.may_yield(l, &o.src.vars)
            {
                let yielding = self.yielding(&mut o.b, ev);
                let (go, stop) = case_zero(&mut o.b, yielding);
                o.b.switch_to(go);
                self.lower_tail(o, &b.tail);
                o.b.switch_to(stop);
                let binds = o.vars(&l.binds);
                forget(&mut o.b, &binds);
                return ret_undefined_any(&mut o.b);
            }
            self.lower_tail(o, &b.tail);
        })
    }

    fn lower_tail(&mut self, o: &mut Out<'_, '_>, t: &Tail) {
        conduit_ir::deep(|| match t {
            Tail::Jump {
                target,
                args,
                source,
            } => {
                o.b.set_source(*source);
                let args = o.atoms(args);
                if *target != Label::RET {
                    let j = o.labels[target].clone();
                    o.b.jump(&j, args);
                } else if o.boxed {
                    let tys = self.tys(&o.src.results);
                    let bx = self.box_vals(&mut o.b, &o.margs, args, &tys);
                    o.b.ret([bx]);
                } else {
                    o.b.ret(args);
                }
            }
            Tail::Unreachable => o.b.unreachable(),
            Tail::Trap => o.b.trap(),
            Tail::Join { rec, defs, body } => {
                let tys: Vec<Vec<Ty>> = (defs.iter())
                    .map(|d| d.params.iter().map(|p| self.ty(o.src.var_ty(*p))).collect())
                    .collect();
                let tys: Vec<&[Ty]> = tys.iter().map(Vec::as_slice).collect();
                let points = o.b.join_group(*rec, &tys);
                let after = o.b.current();
                for (d, j) in defs.iter().zip(&points) {
                    o.bind(&d.params, j.params());
                    if let Some(name) = o.src.debug.labels.get(&d.label) {
                        o.b.name_label(j, name.clone());
                    }
                    o.labels.insert(d.label, j.clone());
                }
                for (d, j) in defs.iter().zip(&points) {
                    o.b.switch_to(j.block());
                    self.block_from(o, &d.body, 0);
                }
                o.b.switch_to(after);
                self.block_from(o, body, 0);
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                let scrut = o.atom(scrut);
                let case = o.b.case(scrut);
                for arm in arms {
                    o.b.arm(case, arm.tag);
                    self.block_from(o, &arm.body, 0);
                }
                if let Some(d) = default {
                    o.b.default(case);
                    self.block_from(o, d, 0);
                }
            }
        })
    }

    /// What follows a site when it isn't yielding: the rest, here or, in
    /// a rest, as a tail call of the site's own rest.
    fn continue_after(&mut self, o: &mut Out<'_, '_>, b: &Block, i: usize, site: &Site) {
        conduit_ir::deep(|| {
            if !o.boxed {
                return self.block_from(o, b, i + 1);
            }
            let ev = o.ev.expect("a rest has evidence");
            let binds = o.vars(&site.binds);
            let tys: Vec<Ty> = (site.binds.iter())
                .map(|x| self.ty(o.src.var_ty(*x)))
                .collect();
            let bx = self.box_vals(&mut o.b, &o.margs, vs(&binds), &tys);
            let mut args = vs(&o.vars(&site.free));
            args.extend([ev, bx, ev].map(Atom::Var));
            self.declare(&mut o.b, site.rest);
            let r = o.b.call1_at(site.rest, o.margs.clone(), args);
            o.b.ret([r]);
        })
    }

    /// A site's way out when yielding: the rest goes on the continuation,
    /// and undefined results back.
    fn yield_block(&mut self, o: &mut Out<'_, '_>, site: &Site) {
        let ev = o.ev.expect("a site has evidence");
        // What the site got is undefined, as what yielded gave it:
        // nothing may drop it.
        let binds = o.vars(&site.binds);
        forget(&mut o.b, &binds);
        let piece = self.rest_closure(o, site);
        self.extend(&mut o.b, ev, piece);
        self.undefined_ret(o);
    }

    /// `ret` with undefined results of the function, or an undefined box
    /// in a rest.
    fn undefined_ret(&self, o: &mut Out<'_, '_>) {
        if o.boxed {
            return ret_undefined_any(&mut o.b);
        }
        let us = undefineds(&mut o.b, &self.tys(&o.src.results));
        o.b.ret(us);
    }

    /// A closure of a site's rest, holding what it needs.
    fn rest_closure(&mut self, o: &mut Out<'_, '_>, site: &Site) -> Var {
        let ev = o.ev.expect("a site has evidence");
        let mut caps = o.vars(&site.free);
        caps.push(ev);
        self.declare(&mut o.b, site.rest);
        (o.b).closure_at(site.rest, o.margs.clone(), Mutability::Mut, caps)
    }

    /// The site at the `i`th `let` of `b`, with its rest, made the first
    /// time it's asked for.
    fn site(&mut self, o: &Out<'_, '_>, b: &Block, i: usize) -> Site {
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
            let decls = Rc::clone(&self.decls);
            let src_ty = |x: &Var| self.ty(o.src.var_ty(*x));
            let mut params: Vec<Ty> = free.iter().map(src_ty).collect();
            params.extend([self.rt.ev_ty(), any(), self.rt.ev_ty()]);
            let name = o.src.debug.name.as_deref().unwrap_or("fn");
            let sig = Sig::new(params, vec![any()]);
            let rb = builder(&decls, &sig, format!("{name}_rest"));
            let mut r = Out::new(o.src, rb, true);
            let ps = r.b.param_vars().to_vec();
            let n = free.len();
            r.bind(&free, &ps[..n]);
            r.ev = Some(ps[n]);
            let tys: Vec<Ty> = binds.iter().map(src_ty).collect();
            let outs = self.unbox_vals(&mut r.b, &r.margs.clone(), ps[n + 1], &tys);
            r.bind(&binds, &outs);
            self.block_from(&mut r, b, i + 1);
            self.finish(rest, r.b);
            site
        })
    }

    /// `perform E.op(args)` at the `i`th `let` of `b`, of an effect that
    /// may yield: a direct call if the nearest handler's clause is
    /// tail-resumptive, or else a yield to it. What follows it is a join
    /// point both ways go to.
    fn perform_site(&mut self, o: &mut Out<'_, '_>, b: &Block, i: usize, e: EffectId, op: u32) {
        conduit_ir::deep(|| {
            let l = &b.lets[i];
            let ev = o.ev.expect("a perform has evidence");
            let (node, slot) = self.lookup(o, e, op);
            let kinds = o.b.get(self.rt.node(), 2, node);
            let kind = o.b.array_get(kinds, slot);
            let site = self.site(o, b, i);
            let tys: Vec<Ty> = (l.binds.iter())
                .map(|x| self.ty(o.src.var_ty(*x)))
                .collect();
            let after = o.b.join(&tys);
            let ways = o.b.current();
            o.bind(&l.binds, after.params());
            o.b.switch_to(after.block());
            if self.is_site(b, i, &o.src.vars) {
                let yielding = self.yielding(&mut o.b, ev);
                let (go, stop) = case_zero(&mut o.b, yielding);
                o.b.switch_to(go);
                self.continue_after(o, b, i, &site);
                o.b.switch_to(stop);
                self.yield_block(o, &site);
            } else {
                self.block_from(o, b, i + 1);
            }
            o.b.switch_to(ways);
            o.b.set_source(l.source);
            let args = o.atoms(&l.args);
            let (slow, fast) = case_zero(&mut o.b, kind);
            // Tail-resumptive: call the clause under the handlers outside it.
            o.b.switch_to(fast);
            let xs = self.fast_call(o, (node, slot), (e, op), args.clone());
            o.b.jump(&after, xs);
            // Otherwise yield to it.
            o.b.switch_to(slow);
            let yc = Runtime::ctor(self.rt.yield_d, 0);
            let st = self.yield_state(&mut o.b, ev);
            let marker = o.b.get(self.rt.node(), 1, node);
            o.b.set(yc, 0, st, marker);
            o.b.set(yc, 1, st, slot);
            let ptys = self.tys(&self.src.effects[&e].ops[op as usize].params);
            let a = self.box_vals(&mut o.b, &o.margs, args, &ptys);
            o.b.set(yc, 2, st, a);
            let piece = self.rest_closure(o, &site);
            let force = self.force_func(&mut o.b, e, op);
            let fz = o.b.closure(force, Mutability::Mut, [piece, ev]);
            o.b.set(yc, 3, st, fz);
            self.undefined_ret(o);
        })
    }

    /// The nearest handler node of `e`, and where the clause for `e.op` is
    /// among its handler's.
    fn lookup(&self, o: &mut Out<'_, '_>, e: EffectId, op: u32) -> (Var, Var) {
        let ev = o.ev.expect("a perform has evidence");
        let n = self.effect_number(e);
        let found = o.b.op_as(
            Op::Call(self.rt.lookup, Vec::new(), Vec::new()),
            [Atom::Var(ev), n.into()],
            vec![self.rt.node_ty(), Ty::I32],
        );
        let slot = o.b.add(found[1], op as i32);
        (found[0], slot)
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
        o: &mut Out<'_, '_>,
        (node, slot): (Var, Var),
        (e, op): (EffectId, u32),
        mut args: Vec<Atom>,
    ) -> Vec<Var> {
        let node_c = self.rt.node();
        let closures = o.b.get(node_c, 3, node);
        let c_any = o.b.array_get(closures, slot);
        let c = o.b.unbox(self.fast_ty(e, op), c_any);
        let outer = o.b.get(node_c, 4, node);
        args.push(outer.into());
        o.b.apply(c, args)
    }

    /// A `let` other than a `perform` that may yield.
    fn lower_let(&mut self, o: &mut Out<'_, '_>, l: &Let) {
        let ev = o.ev;
        let with_ev = |mut args: Vec<Atom>| {
            args.push(Atom::Var(ev.expect("evidence for an op that needs it")));
            args
        };
        let args = o.atoms(&l.args);
        let (op, args) = match &l.op {
            Op::Call(g, margs, _) => match self.internal.get(g) {
                Some(&id) => (Op::Call(id, margs.clone(), Vec::new()), with_ev(args)),
                None => (Op::Call(*g, margs.clone(), Vec::new()), args),
            },
            Op::Closure(g, margs, _) => {
                let id = self.ev_version(&mut o.b, *g);
                (Op::Closure(id, margs.clone(), Vec::new()), args)
            }
            Op::Apply => (Op::Apply, with_ev(args)),
            Op::Perform(e, op) => {
                // Of an effect that never yields: always tail-resumptive.
                let node = self.lookup(o, *e, *op);
                let xs = self.fast_call(o, node, (*e, *op), args);
                return o.bind(&l.binds, &xs);
            }
            Op::Handle(h, _) => return self.handle_site(o, l, *h),
            Op::Resume => {
                let (k, vals) = args.split_first().expect("resume of a continuation");
                let takes = self.cont_takes(o, &l.args[0]);
                let cf = self.const_func(&mut o.b, &takes);
                let t = o.b.closure(cf, Mutability::Mut, vals.to_vec());
                let bx = box_any(&mut o.b, self.thunk_ty(&takes), t);
                (Op::Apply, with_ev(vec![k.clone(), Atom::Var(bx)]))
            }
            Op::Discontinue(e, op) => {
                let (k, rest) = args.split_first().expect("discontinue of a continuation");
                let src_takes = match &l.args[0] {
                    Atom::Var(x) => match o.src.var_ty(*x) {
                        Ty::Cont(sig) => sig.params.clone(),
                        _ => Vec::new(),
                    },
                    Atom::Lit(_) => Vec::new(),
                };
                let takes = self.tys(&src_takes);
                let perf = self.perf[&(*e, *op, src_takes)];
                let id = self.ev_version(&mut o.b, perf);
                let t = o.b.closure(id, Mutability::Mut, rest.to_vec());
                let bx = box_any(&mut o.b, self.thunk_ty(&takes), t);
                (Op::Apply, with_ev(vec![k.clone(), Atom::Var(bx)]))
            }
            Op::Box(t) => (Op::Box(self.ty(t)), args),
            Op::Unbox(t) => (Op::Unbox(self.ty(t)), args),
            // The host gives no evidence, so it gets a closure of the
            // adapter, which makes some.
            Op::HostFunction => {
                let sig = match &l.args[0] {
                    Atom::Var(x) => match o.src.var_ty(*x) {
                        Ty::Fn(sig, _) => (**sig).clone(),
                        _ => panic!("host_function of something that isn't a closure"),
                    },
                    Atom::Lit(_) => panic!("host_function of a literal"),
                };
                let id = self.host_adapter(&mut o.b, &sig);
                let a = o.b.closure(id, Mutability::Mut, args);
                (Op::HostFunction, vec![Atom::Var(a)])
            }
            op => (op.clone(), args),
        };
        let tys: Vec<Ty> = l.binds.iter().map(|x| self.ty(o.src.var_ty(*x))).collect();
        let outs = o.b.op_as(op, args, tys);
        o.bind(&l.binds, &outs);
    }

    /// What continuation `k`, of the source, takes, after lowering.
    fn cont_takes(&self, o: &Out<'_, '_>, k: &Atom) -> Vec<Ty> {
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
    fn handle_site(&mut self, o: &mut Out<'_, '_>, l: &Let, h: HandlerId) {
        let ev = o.ev.expect("a handle has evidence");
        let args = o.atoms(&l.args);
        let (env, body) = args.split_at(args.len() - 1);
        let yc = Runtime::ctor(self.rt.yield_d, 0);
        let st = self.yield_state(&mut o.b, ev);
        let marker = o.b.get(yc, 4, st);
        let next = o.b.add(marker, 1i32);
        o.b.set(yc, 4, st, next);
        let ops: Vec<(EffectId, u32)> = self.src.handlers[&h]
            .clauses()
            .map(|(e, j, _)| (e, j))
            .collect();
        let funcs = &self.handlers[&h];
        let (fast, starts, start_body, drive) = (
            funcs.fast.clone(),
            funcs.starts,
            funcs.start_body,
            funcs.drive,
        );
        let kinds: Vec<Atom> = fast.iter().map(|f| i32::from(f.is_some()).into()).collect();
        let kinds = o.b.array_new(Ty::I32, Mutability::Mut, kinds);
        let mut closures = Vec::new();
        for (i, plain) in fast.iter().enumerate() {
            let boxed = match plain {
                Some(plain) => {
                    let id = self.ev_version(&mut o.b, *plain);
                    let (e, j) = ops[i];
                    let c = o.b.closure(id, Mutability::Mut, env.to_vec());
                    box_any(&mut o.b, self.fast_ty(e, j), c)
                }
                None => box_any(&mut o.b, Ty::I32, 0i32),
            };
            closures.push(boxed);
        }
        let closures = o.b.array_new(any(), Mutability::Mut, closures);
        let starts = o.b.constant(starts);
        let node = o.b.new_object(
            self.rt.node(),
            Mutability::Mut,
            [starts, marker, kinds, closures, ev],
        );
        let start = o.b.closure(start_body, Mutability::Mut, body.to_vec());
        let mut args = vec![Atom::Var(ev)];
        args.extend(env.iter().cloned());
        args.extend([node, start].map(Atom::Var));
        let tys: Vec<Ty> = l.binds.iter().map(|x| self.ty(o.src.var_ty(*x))).collect();
        let outs =
            o.b.op_as(Op::Call(drive, Vec::new(), Vec::new()), args, tys);
        o.bind(&l.binds, &outs);
    }
}
