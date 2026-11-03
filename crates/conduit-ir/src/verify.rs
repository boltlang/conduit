//! Checks that IR follows the rules of `docs/ir.md`: scoping, types,
//! mutability, exhaustive cases, and continuations visibly used more than
//! once.
//!
//! [`verify_func`] checks one function, asking a [`Program`] only for the
//! signatures and declarations it uses, so a frontend can verify each
//! function separately. [`verify_module`] checks a whole [`Module`],
//! declarations included.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::*;

pub(crate) mod typing;

/// The item an [`Error`] is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Item {
    Func(FuncId),
    Data(DataId),
    Extern(ExternId),
    Effect(EffectId),
    Handler(HandlerId),
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Func(id) => write!(f, "{id}"),
            Item::Data(id) => write!(f, "{id}"),
            Item::Extern(id) => write!(f, "{id}"),
            Item::Effect(id) => write!(f, "{id}"),
            Item::Handler(id) => write!(f, "{id}"),
        }
    }
}

/// A broken rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Error {
    pub item: Item,
    /// The source of the nearest `let` or `jump` before the error, if it
    /// has one.
    pub source: Option<SourceId>,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.item)?;
        if let Some(source) = self.source {
            write!(f, " ({source})")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for Error {}

/// Checks every declaration and function of a module.
pub fn verify_module(m: &Module) -> Result<(), Vec<Error>> {
    let mut errors = Vec::new();
    for (&id, decl) in &m.data {
        check_data(m, id, decl, &mut errors);
    }
    for (&id, ext) in &m.externs {
        if let Err(message) = sig_ok(m, &ext.sig) {
            errors.push(decl_error(Item::Extern(id), message));
        }
    }
    for (&id, effect) in &m.effects {
        for (i, sig) in effect.ops.iter().enumerate() {
            if let Err(message) = sig_ok(m, sig) {
                errors.push(decl_error(
                    Item::Effect(id),
                    format!("operation {i}: {message}"),
                ));
            }
        }
    }
    for (&id, handler) in &m.handlers {
        check_handler(m, id, handler, &mut errors);
    }
    for (&id, f) in &m.funcs {
        if let Err(e) = verify_func(m, id, f) {
            errors.extend(e);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Checks one function. Declarations it refers to are assumed to be well
/// formed: [`verify_module`] checks those.
pub fn verify_func<P: Program + ?Sized>(p: &P, id: FuncId, f: &Func) -> Result<(), Vec<Error>> {
    let mut cx = Cx {
        p,
        f,
        item: Item::Func(id),
        errors: Vec::new(),
        source: None,
        bound: vec![false; f.vars.len()],
        in_scope: vec![false; f.vars.len()],
        label_bound: vec![false; f.labels.max(1) as usize],
        label_params: vec![None; f.labels.max(1) as usize],
        known: Vec::new(),
    };
    cx.func();
    if cx.errors.is_empty() {
        Ok(())
    } else {
        Err(cx.errors)
    }
}

fn decl_error(item: Item, message: String) -> Error {
    Error {
        item,
        source: None,
        message,
    }
}

fn check_data<P: Program + ?Sized>(p: &P, id: DataId, decl: &DataDecl, errors: &mut Vec<Error>) {
    for (c, ctor) in decl.ctors.iter().enumerate() {
        for (i, field) in ctor.fields.iter().enumerate() {
            if let Err(message) = ty_ok(p, field, true) {
                errors.push(decl_error(
                    Item::Data(id),
                    format!("field {i} of constructor {c}: {message}"),
                ));
            }
        }
    }
}

fn check_handler<P: Program + ?Sized>(
    p: &P,
    id: HandlerId,
    h: &HandlerDecl,
    errors: &mut Vec<Error>,
) {
    let mut err = |message: String| errors.push(decl_error(Item::Handler(id), message));
    for ty in &h.env {
        if let Err(message) = ty_ok(p, ty, false) {
            err(format!("environment: {message}"));
        }
    }
    let Some(effect) = p.effect(h.effect) else {
        return err(format!("unknown effect {}", h.effect));
    };
    let Some(ret) = p.func_sig(h.ret) else {
        return err(format!("unknown return clause {}", h.ret));
    };
    if !ret.params.starts_with(&h.env) {
        err(format!(
            "return clause {} has type {ret}, which doesn't start with the environment",
            h.ret
        ));
    }
    if h.ops.len() != effect.ops.len() {
        return err(format!(
            "has {} clauses, but {} has {} operations",
            h.ops.len(),
            h.effect,
            effect.ops.len()
        ));
    }
    for (i, (&clause, op)) in h.ops.iter().zip(&effect.ops).enumerate() {
        let Some(sig) = p.func_sig(clause) else {
            err(format!("unknown clause {clause} for operation {i}"));
            continue;
        };
        let k = Ty::Cont(Box::new(Sig {
            params: op.results.clone(),
            results: ret.results.clone(),
        }));
        let mut params = h.env.clone();
        params.extend(op.params.iter().cloned());
        params.push(k);
        let expected = Sig {
            params,
            results: ret.results.clone(),
        };
        if sig != expected {
            err(format!(
                "clause {clause} for operation {i} has type {sig}, but should have {expected}"
            ));
        }
    }
}

fn sub(a: &Ty, b: &Ty) -> bool {
    a.is_subtype(b)
}

/// Checks that a type only refers to known data types, and only uses
/// `Inherit` where it's allowed: in field types and in the element type of
/// an array, outside signatures.
fn ty_ok<P: Program + ?Sized>(p: &P, ty: &Ty, inherit_ok: bool) -> Result<(), String> {
    let mut_ok = |m: &Mutability| {
        if *m == Mutability::Inherit && !inherit_ok {
            Err(format!(
                "`inherit` in {ty} is only allowed in field types and array elements"
            ))
        } else {
            Ok(())
        }
    };
    match ty {
        Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 => Ok(()),
        Ty::Data(d, m) => {
            mut_ok(m)?;
            match p.data(*d) {
                Some(_) => Ok(()),
                None => Err(format!("unknown data type {d}")),
            }
        }
        // An element can have the array's mutability.
        Ty::Array(elem, m) => {
            mut_ok(m)?;
            ty_ok(p, elem, true)
        }
        Ty::Fn(sig, m) => {
            mut_ok(m)?;
            sig_ok(p, sig)
        }
        Ty::Cont(sig) => sig_ok(p, sig),
        Ty::Ref(ty) => ty_ok(p, ty, inherit_ok),
        Ty::Any(m) => mut_ok(m),
    }
}

fn sig_ok<P: Program + ?Sized>(p: &P, sig: &Sig) -> Result<(), String> {
    sig.params
        .iter()
        .chain(&sig.results)
        .try_for_each(|ty| ty_ok(p, ty, false))
}

/// A field or element type in an object of mutability `m`: `Inherit`
/// becomes `m`. An `Inherit` in the element type of an array inside `ty`
/// is that array's, so it stays as it is.
pub fn inherit(ty: &Ty, m: Mutability) -> Ty {
    let pick = |own: Mutability| if own == Mutability::Inherit { m } else { own };
    match ty {
        Ty::Data(d, own) => Ty::Data(*d, pick(*own)),
        Ty::Array(elem, own) => Ty::Array(elem.clone(), pick(*own)),
        Ty::Fn(sig, own) => Ty::Fn(sig.clone(), pick(*own)),
        Ty::Ref(inner) => Ty::Ref(Box::new(inherit(inner, m))),
        Ty::Any(own) => Ty::Any(pick(*own)),
        other => other.clone(),
    }
}

/// The type of the elements of an array of type `ty`, with `Inherit`
/// made the array's mutability. `None` if `ty` isn't an array.
pub fn elem_ty(ty: &Ty) -> Option<Ty> {
    match ty {
        Ty::Array(elem, m) => Some(inherit(elem, *m)),
        _ => None,
    }
}

/// Whether a type is `Imm` all the way down.
pub fn deep_imm<P: Program + ?Sized>(p: &P, ty: &Ty) -> bool {
    deep_imm_in(p, ty, &mut BTreeSet::new())
}

fn deep_imm_in<P: Program + ?Sized>(p: &P, ty: &Ty, seen: &mut BTreeSet<DataId>) -> bool {
    match ty {
        Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 => true,
        Ty::Data(d, Mutability::Imm) => {
            // A data type already being checked is assumed fine, so that
            // recursive types terminate.
            if !seen.insert(*d) {
                return true;
            }
            let Some(decl) = p.data(*d) else { return false };
            decl.ctors
                .iter()
                .flat_map(|c| &c.fields)
                .all(|field| deep_imm_in(p, &inherit(field, Mutability::Imm), seen))
        }
        Ty::Array(elem, Mutability::Imm) => deep_imm_in(p, &inherit(elem, Mutability::Imm), seen),
        Ty::Fn(_, Mutability::Imm) | Ty::Any(Mutability::Imm) => true,
        _ => false,
    }
}

/// The type of `Copy(m)` of a value of type `ty`, or why it can't be
/// copied. The type decides what's copied: a data object or an array whose
/// type is already `m` is kept with everything it holds, and any other one,
/// `ro` included, is copied, with the same rule for what it holds through
/// fields and elements that inherit its mutability. Fields and elements
/// with a mutability of their own are shared, so their types stay.
pub fn copy_ty<P: Program + ?Sized>(p: &P, ty: &Ty, m: Mutability) -> Result<Ty, String> {
    let imm = m == Mutability::Imm;
    match ty {
        Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 => Ok(ty.clone()),
        Ty::Data(d, _) => {
            let copied = Ty::Data(*d, m);
            if imm && !deep_imm(p, &copied) {
                return Err(format!(
                    "can't copy {ty} to imm: it has a field that can't be imm"
                ));
            }
            Ok(copied)
        }
        // An array that's already `m` is kept as it is, with what it holds.
        Ty::Array(_, own) if *own == m => Ok(ty.clone()),
        Ty::Array(elem, _) => {
            let copied = Ty::Array(elem.clone(), m);
            if imm && !deep_imm(p, &copied) {
                return Err(format!("can't copy {ty} to imm: its elements can't be imm"));
            }
            Ok(copied)
        }
        Ty::Fn(_, Mutability::Imm) | Ty::Any(Mutability::Imm) => Ok(ty.clone()),
        Ty::Fn(..) | Ty::Any(_) | Ty::Ref(_) | Ty::Cont(_) if imm => {
            Err(format!("can't copy to imm a value that reaches a {ty}"))
        }
        Ty::Fn(..) | Ty::Any(_) | Ty::Ref(_) | Ty::Cont(_) => Ok(ty.clone()),
    }
}

/// Checks one function.
struct Cx<'a, P: ?Sized> {
    p: &'a P,
    f: &'a Func,
    item: Item,
    errors: Vec<Error>,
    source: Option<SourceId>,
    /// Whether each variable has been bound, to check it's bound once.
    bound: Vec<bool>,
    in_scope: Vec<bool>,
    label_bound: Vec<bool>,
    /// The parameter types of each label in scope.
    label_params: Vec<Option<Vec<Ty>>>,
    /// What the enclosing `case` arms say about variables.
    known: Vec<(Var, CtorId)>,
}

type Checked<T> = Result<T, String>;

impl<P: Program + ?Sized> Cx<'_, P> {
    fn err(&mut self, message: impl Into<String>) {
        self.errors.push(Error {
            item: self.item,
            source: self.source,
            message: message.into(),
        });
    }

    fn func(&mut self) {
        let f = self.f;
        for (i, ty) in f.vars.iter().enumerate() {
            if let Err(message) = ty_ok(self.p, ty, false) {
                self.err(format!("type of %{i}: {message}"));
            }
        }
        for ty in &f.results {
            if let Err(message) = ty_ok(self.p, ty, false) {
                self.err(format!("result type: {message}"));
            }
        }
        if f.labels == 0 {
            self.err("a function has at least one label, `ret`");
        }
        self.label_bound[0] = true;
        self.label_params[0] = Some(f.results.clone());
        for &param in &f.params {
            self.bind(param);
        }
        self.block(&f.body);

        let counts = Linear {
            f,
            labels: BTreeMap::new(),
        }
        .uses(&f.body);
        for (var, n) in counts {
            if n > 1 {
                self.source = None;
                self.err(format!("continuation {var} may be used more than once"));
            }
        }
    }

    fn var_ty(&self, v: Var) -> Option<&Ty> {
        self.f.vars.get(v.0 as usize)
    }

    fn bind(&mut self, v: Var) {
        let i = v.0 as usize;
        if i >= self.f.vars.len() {
            return self.err(format!(
                "{v} has no type: there are {} variables",
                self.f.vars.len()
            ));
        }
        if self.bound[i] {
            self.err(format!("{v} is bound more than once"));
        }
        self.bound[i] = true;
        self.in_scope[i] = true;
    }

    fn unbind(&mut self, v: Var) {
        if let Some(s) = self.in_scope.get_mut(v.0 as usize) {
            *s = false;
        }
    }

    fn atom(&mut self, a: &Atom) -> Option<Ty> {
        match a {
            Atom::Lit(lit) => Some(lit.ty()),
            Atom::Var(v) => {
                let Some(ty) = self.var_ty(*v).cloned() else {
                    self.err(format!(
                        "{v} has no type: there are {} variables",
                        self.f.vars.len()
                    ));
                    return None;
                };
                if !self.in_scope[v.0 as usize] {
                    self.err(format!("{v} is used where it isn't in scope"));
                    return None;
                }
                Some(ty)
            }
        }
    }

    /// The types of atoms, or `None` if any of them is broken (which has
    /// been reported).
    fn atoms(&mut self, atoms: &[Atom]) -> Option<Vec<Ty>> {
        let tys: Vec<_> = atoms.iter().map(|a| self.atom(a)).collect();
        tys.into_iter().collect()
    }

    fn block(&mut self, b: &Block) {
        for l in &b.lets {
            self.source = l.source;
            if let Some(tys) = self.atoms(&l.args) {
                self.op(&l.op, &l.args, &tys, &l.binds);
            }
            for &v in &l.binds {
                self.bind(v);
            }
        }
        match &b.tail {
            Tail::Join { rec, defs, body } => self.join(*rec, defs, body),
            Tail::Jump {
                target,
                args,
                source,
            } => {
                self.source = *source;
                self.jump(*target, args);
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => self.case(scrut, arms, default.as_deref()),
            Tail::Unreachable => {}
        }
        for l in &b.lets {
            for &v in &l.binds {
                self.unbind(v);
            }
        }
    }

    fn join(&mut self, rec: bool, defs: &[JoinDef], body: &Block) {
        let mut labels = Vec::new();
        for def in defs {
            let l = def.label;
            if l == Label::RET || l.0 >= self.f.labels {
                self.err(format!(
                    "{l} can't be bound: labels are j1 to j{}",
                    self.f.labels.saturating_sub(1)
                ));
                continue;
            }
            if self.label_bound[l.0 as usize] {
                self.err(format!("{l} is bound more than once"));
                continue;
            }
            self.label_bound[l.0 as usize] = true;
            let params = def
                .params
                .iter()
                .filter_map(|&p| self.var_ty(p).cloned())
                .collect();
            labels.push((l, params));
        }

        if rec {
            self.scope_labels(&labels);
        }
        for def in defs {
            for &p in &def.params {
                self.bind(p);
            }
            self.block(&def.body);
            for &p in &def.params {
                self.unbind(p);
            }
        }
        if !rec {
            self.scope_labels(&labels);
        }
        self.block(body);
        for (l, _) in labels {
            self.label_params[l.0 as usize] = None;
        }
    }

    fn scope_labels(&mut self, labels: &[(Label, Vec<Ty>)]) {
        for (l, params) in labels {
            self.label_params[l.0 as usize] = Some(params.clone());
        }
    }

    fn jump(&mut self, target: Label, args: &[Atom]) {
        let params = self.label_params.get(target.0 as usize).cloned().flatten();
        let Some(params) = params else {
            return self.err(format!("jump to {target}, which isn't in scope"));
        };
        let Some(tys) = self.atoms(args) else { return };
        if let Err(message) = fits(&tys, &params) {
            self.err(format!("jump to {target}: {message}"));
        }
    }

    fn case(&mut self, scrut: &Atom, arms: &[Arm], default: Option<&Block>) {
        if let Some(ty) = self.atom(scrut) {
            self.case_tags(&ty, arms, default.is_some());
        }
        for arm in arms {
            let fact = match (scrut, arm.tag) {
                (Atom::Var(v), Tag::Ctor(c)) => Some((*v, c)),
                _ => None,
            };
            if let Some(fact) = fact {
                self.known.push(fact);
            }
            self.block(&arm.body);
            if fact.is_some() {
                self.known.pop();
            }
        }
        if let Some(default) = default {
            self.block(default);
        }
    }

    fn case_tags(&mut self, ty: &Ty, arms: &[Arm], has_default: bool) {
        match ty {
            Ty::I32 | Ty::I64 => {
                let mut seen = BTreeSet::new();
                for arm in arms {
                    match arm.tag {
                        Tag::Int(n) if *ty == Ty::I32 && i32::try_from(n).is_err() => {
                            self.err(format!("case on an i32 has a tag {n}, which isn't an i32"));
                        }
                        Tag::Int(n) if !seen.insert(n) => {
                            self.err(format!("case has the tag {n} more than once"));
                        }
                        Tag::Int(_) => {}
                        Tag::Ctor(c) => self.err(format!("case on {ty} has a constructor tag {c}")),
                    }
                }
                if !has_default {
                    self.err(format!("case on {ty} has no default"));
                }
            }
            Ty::Data(d, _) => {
                let Some(decl) = self.p.data(*d) else { return };
                let ctors = decl.ctors.len();
                let mut seen = BTreeSet::new();
                for arm in arms {
                    match arm.tag {
                        Tag::Ctor(c) if c.data != *d || c.index as usize >= ctors => {
                            self.err(format!(
                                "case on {ty} has the tag {c}, which isn't one of its constructors"
                            ));
                        }
                        Tag::Ctor(c) if !seen.insert(c.index) => {
                            self.err(format!("case has the tag {c} more than once"));
                        }
                        Tag::Ctor(_) => {}
                        Tag::Int(n) => self.err(format!("case on {ty} has an integer tag {n}")),
                    }
                }
                if !has_default && seen.len() < ctors {
                    self.err(format!(
                        "case on {ty} has no default and doesn't cover every constructor"
                    ));
                }
            }
            _ => self.err(format!("can't branch on a value of type {ty}")),
        }
    }

    fn op(&mut self, op: &Op, args: &[Atom], tys: &[Ty], binds: &[Var]) {
        let out: Option<Vec<Ty>> = binds.iter().map(|&b| self.var_ty(b).cloned()).collect();
        let Some(out) = out else {
            return self.err("binds a variable that has no type");
        };
        let results = self.op_results(op, args, tys, &out);
        match results {
            Ok(results) if results.len() != out.len() => self.err(format!(
                "binds {} values, but the op gives {}",
                out.len(),
                results.len()
            )),
            Ok(results) => {
                for ((r, o), b) in results.iter().zip(&out).zip(binds) {
                    if !sub(r, o) {
                        self.err(format!("the op gives a {r}, but {b} has type {o}"));
                    }
                }
            }
            Err(message) => self.err(message),
        }
    }

    /// The types an op gives: see [`typing::op_results`].
    fn op_results(&self, op: &Op, args: &[Atom], tys: &[Ty], out: &[Ty]) -> Checked<Vec<Ty>> {
        typing::op_results(self.p, op, args, tys, out, &self.known)
    }
}

/// Objects are created, and copied, as `Imm` or `Mut`, not as a view.
fn object_mutability(m: Mutability) -> Checked<()> {
    match m {
        Mutability::Imm | Mutability::Mut => Ok(()),
        _ => Err(format!("a new object is imm or mut, not {m}")),
    }
}

/// Checks that values of types `tys` can be passed where `expected` are.
pub(crate) fn fits(tys: &[Ty], expected: &[Ty]) -> Checked<()> {
    if tys.len() != expected.len() {
        return Err(format!(
            "expected {} values, but got {}",
            expected.len(),
            tys.len()
        ));
    }
    for (i, (ty, e)) in tys.iter().zip(expected).enumerate() {
        if !sub(ty, e) {
            return Err(format!("value {i} has type {ty}, but should have {e}"));
        }
    }
    Ok(())
}

fn prim_results(prim: &Prim, tys: &[Ty]) -> Checked<Vec<Ty>> {
    let int = |t: &Ty| matches!(t, Ty::I32 | Ty::I64);
    let float = |t: &Ty| matches!(t, Ty::F32 | Ty::F64);
    let num = |t: &Ty| int(t) || float(t);
    let binary = |ok: &dyn Fn(&Ty) -> bool, what: &str| match tys {
        [a, b] if a == b && ok(a) => Ok(a.clone()),
        _ => Err(format!(
            "{prim:?} needs two {what} of the same type, but got ({})",
            list(tys)
        )),
    };
    let unary = |ok: &dyn Fn(&Ty) -> bool, what: &str| match tys {
        [a] if ok(a) => Ok(a.clone()),
        _ => Err(format!(
            "{prim:?} needs one {what}, but got ({})",
            list(tys)
        )),
    };
    let ty = match prim {
        Prim::Add | Prim::Sub | Prim::Mul | Prim::Div(_) => binary(&num, "numbers")?,
        Prim::Rem(_) | Prim::And | Prim::Or | Prim::Xor | Prim::Shl | Prim::Shr(_) => {
            binary(&int, "integers")?
        }
        Prim::Min | Prim::Max => binary(&float, "floats")?,
        Prim::Eq | Prim::Ne | Prim::Lt(_) | Prim::Le(_) | Prim::Gt(_) | Prim::Ge(_) => {
            binary(&num, "numbers")?;
            Ty::I32
        }
        Prim::Neg => unary(&num, "number")?,
        Prim::Abs | Prim::Sqrt => unary(&float, "float")?,
        Prim::Convert(to, _) => {
            unary(&num, "number")?;
            if !num(to) {
                return Err(format!("can't convert to {to}"));
            }
            to.clone()
        }
        Prim::Reinterpret(to) => {
            let from = unary(&num, "number")?;
            let size = |t: &Ty| matches!(t, Ty::I64 | Ty::F64);
            if !num(to) || int(&from) == int(to) || size(&from) != size(to) {
                return Err(format!("can't reinterpret a {from} as a {to}"));
            }
            to.clone()
        }
    };
    Ok(vec![ty])
}

fn list(tys: &[Ty]) -> String {
    tys.iter().map(Ty::to_string).collect::<Vec<_>>().join(", ")
}

/// How many times each continuation may be used along one path, capped at 2.
type Counts = BTreeMap<Var, u8>;

fn add(a: &mut Counts, b: &Counts) {
    for (&v, &n) in b {
        let c = a.entry(v).or_default();
        *c = c.saturating_add(n).min(2);
    }
}

fn max_into(a: &mut Counts, b: &Counts) {
    for (&v, &n) in b {
        let c = a.entry(v).or_default();
        *c = (*c).max(n);
    }
}

/// Counts uses of continuations along paths. A jump to a join point counts
/// what the join point's body uses, so a continuation used before a jump
/// and in the join point counts twice. Recursive join points are counted
/// until their counts stop changing.
///
/// This only sees direct uses: a continuation stored in a closure, a field,
/// an array, a ref or an `Any` can be used again through it, which the
/// run-time check catches instead (rule 7 in `docs/ir.md`). And it doesn't
/// know which paths can run, so branches on the same condition count as if
/// both could be taken.
struct Linear<'a> {
    f: &'a Func,
    /// What each join point's body uses, of variables bound outside it.
    labels: BTreeMap<Label, Counts>,
}

impl Linear<'_> {
    fn atoms<'b>(&self, atoms: impl IntoIterator<Item = &'b Atom>) -> Counts {
        let mut counts = Counts::new();
        for a in atoms {
            if let Atom::Var(v) = a
                && matches!(self.f.vars.get(v.0 as usize), Some(Ty::Cont(_)))
            {
                add(&mut counts, &Counts::from([(*v, 1)]));
            }
        }
        counts
    }

    fn uses(&mut self, b: &Block) -> Counts {
        let mut c = Counts::new();
        for l in &b.lets {
            add(&mut c, &self.atoms(&l.args));
        }
        let tail = match &b.tail {
            Tail::Jump { target, args, .. } => {
                let mut c = self.atoms(args);
                if let Some(l) = self.labels.get(target) {
                    add(&mut c, l);
                }
                c
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                let mut paths = Counts::new();
                for arm in arms {
                    max_into(&mut paths, &self.uses(&arm.body));
                }
                if let Some(default) = default {
                    max_into(&mut paths, &self.uses(default));
                }
                let mut c = self.atoms([scrut]);
                add(&mut c, &paths);
                c
            }
            Tail::Join { rec, defs, body } => {
                if *rec {
                    for def in defs {
                        self.labels.insert(def.label, Counts::new());
                    }
                    // Counts only grow and are capped, so this ends.
                    loop {
                        let mut changed = false;
                        for def in defs {
                            let c = self.def_uses(def);
                            if self.labels.get(&def.label) != Some(&c) {
                                self.labels.insert(def.label, c);
                                changed = true;
                            }
                        }
                        if !changed {
                            break;
                        }
                    }
                } else {
                    for def in defs {
                        let c = self.def_uses(def);
                        self.labels.insert(def.label, c);
                    }
                }
                self.uses(body)
            }
            Tail::Unreachable => Counts::new(),
        };
        add(&mut c, &tail);
        c
    }

    /// What a join point's body uses, of variables bound outside it: those
    /// bound inside are new each time the join point is entered.
    fn def_uses(&mut self, def: &JoinDef) -> Counts {
        let mut c = self.uses(&def.body);
        let mut inner = BTreeSet::from_iter(def.params.iter().copied());
        binds(&def.body, &mut inner);
        c.retain(|v, _| !inner.contains(v));
        c
    }
}

/// Every variable a block binds.
fn binds(b: &Block, out: &mut BTreeSet<Var>) {
    for l in &b.lets {
        out.extend(l.binds.iter().copied());
    }
    match &b.tail {
        Tail::Join { defs, body, .. } => {
            for def in defs {
                out.extend(def.params.iter().copied());
                binds(&def.body, out);
            }
            binds(body, out);
        }
        Tail::Case { arms, default, .. } => {
            for arm in arms {
                binds(&arm.body, out);
            }
            if let Some(default) = default {
                binds(default, out);
            }
        }
        Tail::Jump { .. } | Tail::Unreachable => {}
    }
}

#[cfg(test)]
mod tests;
