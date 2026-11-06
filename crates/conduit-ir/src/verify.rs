//! Checks that IR follows the rules of `docs/ir.md`, as `docs/verify.md`
//! lists them: scoping, types, mutability, exhaustive cases, and
//! continuations visibly used more than once.
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
    Const(ConstId),
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Func(id) => write!(f, "{id}"),
            Item::Data(id) => write!(f, "{id}"),
            Item::Extern(id) => write!(f, "{id}"),
            Item::Effect(id) => write!(f, "{id}"),
            Item::Handler(id) => write!(f, "{id}"),
            Item::Const(id) => write!(f, "{id}"),
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

impl Error {
    /// The error as [`Display`](fmt::Display) writes it, with the name of
    /// its item in `p` before its number, like `sum (fn3): ..`, if it has
    /// one.
    pub fn named<P: Program + ?Sized>(&self, p: &P) -> String {
        let name = match self.item {
            Item::Func(id) => p.func(id).and_then(|f| f.debug.name.clone()),
            Item::Data(id) => p.data(id).and_then(|d| d.debug_name.clone()),
            Item::Extern(id) => p.extern_fn(id).and_then(|e| e.debug_name.clone()),
            Item::Effect(id) => p.effect(id).and_then(|e| e.debug_name.clone()),
            Item::Handler(id) => p.handler(id).and_then(|h| h.debug_name.clone()),
            Item::Const(id) => p.constant(id).and_then(|c| c.debug_name.clone()),
        };
        let Some(name) = name else {
            return self.to_string();
        };
        let source = self.source.map_or(String::new(), |s| format!(" ({s})"));
        format!("{name} ({}){source}: {}", self.item, self.message)
    }
}

/// Checks every declaration and function of a module.
pub fn verify_module(m: &Module) -> Result<(), Vec<Error>> {
    let mut errors = Vec::new();
    for (&id, decl) in &m.data {
        check_data(m, id, decl, &mut errors);
    }
    for (&id, ext) in &m.externs {
        if let Err(message) = sig_ok(m, &ext.sig, 0, 0).and_then(|()| no_row(&ext.sig)) {
            errors.push(decl_error(Item::Extern(id), message));
        }
    }
    for (&id, effect) in &m.effects {
        for (i, sig) in effect.ops.iter().enumerate() {
            if let Err(message) = sig_ok(m, sig, 0, 0).and_then(|()| no_row(sig)) {
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
    for (&id, decl) in &m.consts {
        if let Err(message) = check_const(m, decl) {
            errors.push(decl_error(Item::Const(id), message));
        }
    }
    if let Some(id) = const_cycle(m) {
        errors.push(decl_error(
            Item::Const(id),
            format!("{id} refers to itself, through the constants it's made of"),
        ));
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
            let ok = no_token(field)
                .and_then(|()| ty_ok(p, field, Allowed::field(decl.params, decl.effect_params)));
            if let Err(message) = ok {
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
    for clause in h.ops.iter().chain([&h.ret]) {
        if p.func_mut_params(*clause).is_some_and(|mp| mp.count > 0) {
            err(format!(
                "clause {clause} has mutability parameters, which a handler can't give"
            ));
        }
        // A clause's effect parameters are the handler's.
        if let Some(n) = p.func_effect_params(*clause)
            && n != h.effect_params
        {
            err(format!(
                "clause {clause} has {n} effect parameters, but the handler has {}",
                h.effect_params
            ));
        }
    }
    let allowed = Allowed::func(0, h.effect_params);
    for ty in &h.env {
        if let Err(message) = ty_ok(p, ty, allowed) {
            err(format!("environment: {message}"));
        }
    }
    let Some(effect) = p.effect(h.effect) else {
        return err(format!("unknown effect {}", h.effect));
    };
    if let Err(message) = row_ok(p, &h.row, h.effect_params) {
        err(message);
    }
    let Some(ret) = p.func_sig(h.ret) else {
        return err(format!("unknown return clause {}", h.ret));
    };
    if !ret.row.is_in(&h.row) {
        err(format!(
            "return clause {} does {}, which isn't in the handler's row {}",
            h.ret, ret.row, h.row
        ));
    }
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
        // Resuming runs the rest of the body, which is still handled.
        let k = Ty::Cont(Box::new(Sig {
            row: h.row.clone(),
            params: op.results.clone(),
            results: ret.results.clone(),
        }));
        let mut params = h.env.clone();
        params.extend(op.params.iter().cloned());
        params.push(k);
        let expected = Sig {
            row: h.row.clone(),
            params,
            results: ret.results.clone(),
        };
        if sig.params != expected.params || sig.results != expected.results {
            err(format!(
                "clause {clause} for operation {i} has type {sig}, but should have {expected}"
            ));
        } else if !sig.row.is_in(&h.row) {
            err(format!(
                "clause {clause} for operation {i} does {}, which isn't in the handler's row {}",
                sig.row, h.row
            ));
        }
    }
}

/// Checks a constant's type, and that its value is one of that type.
fn check_const<P: Program + ?Sized>(p: &P, decl: &ConstDecl) -> Result<(), String> {
    ty_ok(p, &decl.ty, Allowed::PLAIN)?;
    if !deep_imm(p, &decl.ty) {
        return Err(format!(
            "a constant is imm all the way down, but {} isn't",
            decl.ty
        ));
    }
    const_value_ok(p, &decl.value, &decl.ty)
}

/// Checks that a constant's value, or part of one, is of type `ty`, which
/// is `Imm` all the way down.
fn const_value_ok<P: Program + ?Sized>(p: &P, value: &ConstValue, ty: &Ty) -> Result<(), String> {
    match (value, ty) {
        (ConstValue::Lit(lit), _) if lit.ty() == *ty => Ok(()),
        (ConstValue::Lit(lit), _) => Err(format!(
            "{} is a {}, not a {ty}",
            text::atom_text(&Atom::Lit(*lit)),
            lit.ty()
        )),
        (ConstValue::Array(elems), Ty::Array(..)) => {
            let elem = elem_ty(ty).unwrap();
            elems.iter().enumerate().try_for_each(|(i, e)| {
                const_value_ok(p, e, &elem).map_err(|m| format!("element {i}: {m}"))
            })
        }
        (ConstValue::Array(_), _) => Err(format!("an array isn't a {ty}")),
        (ConstValue::New(c, fields), Ty::Data(d, m, args, rows)) if c.data == *d => {
            let decl = p.data(*d).ok_or_else(|| format!("unknown data type {d}"))?;
            let ctor = decl
                .ctors
                .get(c.index as usize)
                .ok_or_else(|| format!("unknown constructor {c}"))?;
            if ctor.fields.len() != fields.len() {
                return Err(format!(
                    "{c} has {} fields, but gets {}",
                    ctor.fields.len(),
                    fields.len()
                ));
            }
            ctor.fields
                .iter()
                .zip(fields)
                .enumerate()
                .try_for_each(|(i, (field, value))| {
                    let field = resolve(field, *m, args, rows);
                    const_value_ok(p, value, &field).map_err(|m| format!("field {i} of {c}: {m}"))
                })
        }
        (ConstValue::New(c, _), _) => Err(format!("{c} doesn't build a {ty}")),
        (ConstValue::Const(id), _) => {
            let decl = p
                .constant(*id)
                .ok_or_else(|| format!("unknown constant {id}"))?;
            if sub(&decl.ty, ty) {
                Ok(())
            } else {
                Err(format!("{id} is a {}, not a {ty}", decl.ty))
            }
        }
    }
}

/// A constant that refers to itself through others, if there's one.
fn const_cycle(m: &Module) -> Option<ConstId> {
    // 0: not visited, 1: being visited, 2: done.
    let mut state: BTreeMap<ConstId, u8> = BTreeMap::new();
    for &start in m.consts.keys() {
        if state.contains_key(&start) {
            continue;
        }
        // A depth-first walk, with the constants each one still has to
        // visit.
        let mut stack = vec![(start, const_refs(&m.consts[&start].value))];
        state.insert(start, 1);
        while let Some((id, refs)) = stack.last_mut() {
            let Some(next) = refs.pop() else {
                state.insert(*id, 2);
                stack.pop();
                continue;
            };
            match state.get(&next) {
                Some(1) => return Some(next),
                Some(_) => {}
                None => {
                    let Some(decl) = m.consts.get(&next) else {
                        continue;
                    };
                    state.insert(next, 1);
                    stack.push((next, const_refs(&decl.value)));
                }
            }
        }
    }
    None
}

/// What constant `id` is, past the constants that are only another name
/// for one: `None` if one of them is unknown, or they refer to each other
/// in a cycle.
pub fn const_value<P: Program + ?Sized>(p: &P, id: ConstId) -> Option<&ConstValue> {
    let mut seen = BTreeSet::new();
    let mut value = &p.constant(id)?.value;
    while let ConstValue::Const(next) = value {
        if !seen.insert(*next) {
            return None;
        }
        value = &p.constant(*next)?.value;
    }
    Some(value)
}

/// The constants a value refers to.
pub fn const_refs(value: &ConstValue) -> Vec<ConstId> {
    let mut refs = Vec::new();
    let mut todo = vec![value];
    while let Some(v) = todo.pop() {
        match v {
            ConstValue::Lit(_) => {}
            ConstValue::Array(parts) | ConstValue::New(_, parts) => todo.extend(parts),
            ConstValue::Const(id) => refs.push(*id),
        }
    }
    refs
}

fn sub(a: &Ty, b: &Ty) -> bool {
    a.is_subtype(b)
}

/// Where [`ty_ok`] checks a type: which mutabilities it may use besides
/// `Imm`, `Mut` and `ReadOnly`.
#[derive(Clone, Copy)]
struct Allowed {
    /// `Inherit`: in field types, and in an array's element type.
    inherit: bool,
    /// `Param(1)` to `Param(params)`: in the field types of a declaration
    /// with that many parameters, or the types of a function.
    params: u32,
    /// Effect parameters `E1` to `E{rows}` in rows: those of the data
    /// declaration, function or handler the type is in.
    rows: u32,
}

impl Allowed {
    /// A type outside data declarations and functions: an extern's, an
    /// effect's, a handler's.
    const PLAIN: Allowed = Allowed {
        inherit: false,
        params: 0,
        rows: 0,
    };

    /// A type in a function with `params` mutability parameters and
    /// `rows` effect parameters: a variable's, a result's, or one an op
    /// takes.
    pub(crate) fn func(params: u32, rows: u32) -> Allowed {
        Allowed {
            inherit: false,
            params,
            rows,
        }
    }

    /// A field type of a declaration with `params` mutability parameters
    /// and `rows` effect parameters.
    fn field(params: u32, rows: u32) -> Allowed {
        Allowed {
            inherit: true,
            params,
            rows,
        }
    }
}

/// Checks that a type only refers to known data types, gives each the
/// right number of arguments, and only uses `Inherit` and mutability
/// parameters where they're allowed.
fn ty_ok<P: Program + ?Sized>(p: &P, ty: &Ty, allowed: Allowed) -> Result<(), String> {
    let mut_ok = |m: &Mutability| match m {
        Mutability::Inherit if !allowed.inherit => Err(format!(
            "`inherit` in {ty} is only allowed in field types and array elements"
        )),
        Mutability::Param(0) => Err(format!("{ty} has a parameter 0, which is `inherit`")),
        Mutability::Param(i) if *i > allowed.params => Err(format!(
            "{m} in {ty} isn't a parameter here: there {}",
            match allowed.params {
                0 => "are none".to_string(),
                1 => "is 'm1".to_string(),
                n => format!("are 'm1 to 'm{n}"),
            }
        )),
        _ => Ok(()),
    };
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 => Ok(()),
        Ty::Data(d, m, args, rows) => {
            mut_ok(m)?;
            let Some(decl) = p.data(*d) else {
                return Err(format!("unknown data type {d}"));
            };
            if args.len() != decl.params as usize {
                return Err(format!(
                    "{d} takes {} mutability arguments, but {ty} gives {}",
                    decl.params,
                    args.len()
                ));
            }
            if rows.len() != decl.effect_params as usize {
                return Err(format!(
                    "{d} takes {} rows, but {ty} gives {}",
                    decl.effect_params,
                    rows.len()
                ));
            }
            args.iter().try_for_each(mut_ok)?;
            rows.iter().try_for_each(|r| row_ok(p, r, allowed.rows))
        }
        // An element can have the array's mutability.
        Ty::Array(elem, m) => {
            mut_ok(m)?;
            no_token(elem)?;
            ty_ok(
                p,
                elem,
                Allowed {
                    inherit: true,
                    ..allowed
                },
            )
        }
        Ty::Fn(sig, m) => {
            mut_ok(m)?;
            sig_ok(p, sig, allowed.params, allowed.rows)?;
            row_ok(p, &sig.row, allowed.rows)
        }
        Ty::Cont(sig) => {
            sig_ok(p, sig, allowed.params, allowed.rows)?;
            row_ok(p, &sig.row, allowed.rows)
        }
        Ty::Ref(ty) => {
            no_token(ty)?;
            ty_ok(p, ty, Allowed { ..allowed })
        }
        Ty::Any(m) => mut_ok(m),
        Ty::Reuse(c) => match p.data(c.data).and_then(|d| d.ctors.get(c.index as usize)) {
            Some(_) => Ok(()),
            None => Err(format!("unknown constructor {c}")),
        },
    }
}

/// Whether objects of constructors `a` and `b` have the same shape, so
/// that one can be built in the other's memory: as many fields, each an
/// object in both, or of the same scalar type. Such objects are the same
/// size in every backend.
pub fn same_shape<P: Program + ?Sized>(p: &P, a: CtorId, b: CtorId) -> bool {
    let fields = |c: CtorId| p.data(c.data).and_then(|d| d.ctors.get(c.index as usize));
    let (Some(a), Some(b)) = (fields(a), fields(b)) else {
        return false;
    };
    let scalar = |t: &Ty| matches!(t, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64);
    a.fields.len() == b.fields.len()
        && a.fields
            .iter()
            .zip(&b.fields)
            .all(|(x, y)| match (scalar(x), scalar(y)) {
                (false, false) => true,
                (true, true) => x == y,
                _ => false,
            })
}

/// Fails if `ty` is a reuse token, which only a variable can hold.
fn no_token(ty: &Ty) -> Result<(), String> {
    match ty {
        Ty::Reuse(_) => Err(format!("{ty} can only be a variable's type")),
        _ => Ok(()),
    }
}

/// Checks that a row's effects exist, and that it only uses effect
/// parameters `E1` to `E{params}`.
pub(crate) fn row_ok<P: Program + ?Sized>(p: &P, row: &Row, params: u32) -> Result<(), String> {
    if let Some(e) = row.effects().iter().find(|&&e| p.effect(e).is_none()) {
        return Err(format!("unknown effect {e} in row {row}"));
    }
    match row.params().iter().find(|&&i| i == 0 || i > params) {
        Some(i) => Err(format!(
            "E{i} in row {row} isn't an effect parameter here: there {}",
            match params {
                0 => "are none".to_string(),
                1 => "is E1".to_string(),
                n => format!("are E1 to E{n}"),
            }
        )),
        None => Ok(()),
    }
}

/// An extern's or an effect operation's signature has no row: the host
/// can't perform a Conduit effect, and an operation's effects are its
/// handler's.
fn no_row(sig: &Sig) -> Result<(), String> {
    if sig.row.is_nothing() {
        Ok(())
    } else {
        Err(format!(
            "{sig} has a row, which only functions, closures and continuations can"
        ))
    }
}

/// Checks a signature's types, which can use mutability parameters 1 to
/// `params` and effect parameters 1 to `rows`, but not `Inherit`.
fn sig_ok<P: Program + ?Sized>(p: &P, sig: &Sig, params: u32, rows: u32) -> Result<(), String> {
    sig.params
        .iter()
        .chain(&sig.results)
        .try_for_each(|ty| ty_ok(p, ty, Allowed::func(params, rows)))
}

/// A field or element type in an object of mutability `m`: `Inherit`
/// becomes `m`. An `Inherit` in the element type of an array inside `ty`
/// is that array's, so it stays as it is.
pub fn inherit(ty: &Ty, m: Mutability) -> Ty {
    let pick = |own: Mutability| if own == Mutability::Inherit { m } else { own };
    match ty {
        Ty::Data(d, own, args, rows) => Ty::Data(
            *d,
            pick(*own),
            args.iter().map(|&a| pick(a)).collect(),
            rows.clone(),
        ),
        Ty::Array(elem, own) => Ty::Array(elem.clone(), pick(*own)),
        Ty::Fn(sig, own) => Ty::Fn(sig.clone(), pick(*own)),
        Ty::Ref(inner) => Ty::Ref(Box::new(inherit(inner, m))),
        Ty::Any(own) => Ty::Any(pick(*own)),
        other => other.clone(),
    }
}

/// `ty` with each mutability parameter `Param(i)` replaced by `args[i - 1]`,
/// wherever it is.
pub fn subst_params(ty: &Ty, args: &[Mutability]) -> Ty {
    if args.is_empty() {
        return ty.clone();
    }
    let pick = |m: Mutability| match m {
        Mutability::Param(i) => param_arg(args, i).unwrap_or(m),
        m => m,
    };
    match ty {
        Ty::Data(d, own, a, rows) => Ty::Data(
            *d,
            pick(*own),
            a.iter().map(|&m| pick(m)).collect(),
            rows.clone(),
        ),
        Ty::Array(elem, own) => Ty::Array(Box::new(subst_params(elem, args)), pick(*own)),
        Ty::Fn(sig, own) => Ty::Fn(Box::new(subst_sig(sig, args)), pick(*own)),
        Ty::Cont(sig) => Ty::Cont(Box::new(subst_sig(sig, args))),
        Ty::Ref(inner) => Ty::Ref(Box::new(subst_params(inner, args))),
        Ty::Any(own) => Ty::Any(pick(*own)),
        other => other.clone(),
    }
}

/// The argument for parameter `Param(i)`, if there is one.
fn param_arg(args: &[Mutability], i: u32) -> Option<Mutability> {
    args.get((i as usize).checked_sub(1)?).copied()
}

/// `sig` with each mutability parameter `Param(i)` replaced by
/// `args[i - 1]`: a function's signature as a call with those arguments
/// sees it.
pub fn subst_sig(sig: &Sig, args: &[Mutability]) -> Sig {
    let all = |tys: &[Ty]| tys.iter().map(|ty| subst_params(ty, args)).collect();
    Sig {
        row: sig.row.clone(),
        params: all(&sig.params),
        results: all(&sig.results),
    }
}

/// `ty` with each effect parameter `Ei` in its rows replaced by
/// `rows[i - 1]`, wherever it is.
pub fn subst_rows(ty: &Ty, rows: &[Row]) -> Ty {
    if rows.is_empty() {
        return ty.clone();
    }
    match ty {
        Ty::Data(d, own, a, r) => Ty::Data(
            *d,
            *own,
            a.clone(),
            r.iter().map(|r| r.subst(rows)).collect(),
        ),
        Ty::Array(elem, own) => Ty::Array(Box::new(subst_rows(elem, rows)), *own),
        Ty::Fn(sig, own) => Ty::Fn(Box::new(subst_sig_rows(sig, rows)), *own),
        Ty::Cont(sig) => Ty::Cont(Box::new(subst_sig_rows(sig, rows))),
        Ty::Ref(inner) => Ty::Ref(Box::new(subst_rows(inner, rows))),
        other => other.clone(),
    }
}

/// `sig` with each effect parameter `Ei` replaced by `rows[i - 1]`, in its
/// row and its types.
pub fn subst_sig_rows(sig: &Sig, rows: &[Row]) -> Sig {
    let all = |tys: &[Ty]| tys.iter().map(|ty| subst_rows(ty, rows)).collect();
    Sig {
        row: sig.row.subst(rows),
        params: all(&sig.params),
        results: all(&sig.results),
    }
}

/// `ty` with both kinds of parameter replaced: mutability parameters by
/// `margs`, effect parameters by `rows`.
pub fn subst_all(ty: &Ty, margs: &[Mutability], rows: &[Row]) -> Ty {
    subst_rows(&subst_params(ty, margs), rows)
}

/// The type of a field declared as `field` in an object of type
/// `Data(_, m, args, rows)`: the arguments in place of the parameters, and
/// `m` in place of `Inherit`.
pub fn resolve(field: &Ty, m: Mutability, args: &[Mutability], rows: &[Row]) -> Ty {
    inherit(&subst_all(field, args, rows), m)
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
    at_most(p, ty, Mutability::Imm, &[])
}

/// Whether everything a value of type `ty` reaches is at most `bound`
/// (see [`Mutability::at_most`]), so that an object of mutability `bound`
/// can hold it. A ref or a continuation can change, so it's at most `Mut`
/// only. With `bound` `Imm`, this is [`deep_imm`].
pub fn at_most<P: Program + ?Sized>(
    p: &P,
    ty: &Ty,
    bound: Mutability,
    assumes: &[(Mutability, Mutability)],
) -> bool {
    at_most_in(p, ty, bound, assumes, &mut BTreeSet::new())
}

fn at_most_in<P: Program + ?Sized>(
    p: &P,
    ty: &Ty,
    bound: Mutability,
    assumes: &[(Mutability, Mutability)],
    seen: &mut BTreeSet<(DataId, Mutability, Vec<Mutability>)>,
) -> bool {
    let le = |m: Mutability| m.at_most(bound, assumes);
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 => true,
        Ty::Data(d, m, args, rows) => {
            if !le(*m) {
                return false;
            }
            // A data type already being checked is assumed fine, so that
            // recursive types terminate. Rows don't change mutabilities.
            if !seen.insert((*d, *m, args.clone())) {
                return true;
            }
            let Some(decl) = p.data(*d) else { return false };
            decl.ctors
                .iter()
                .flat_map(|c| &c.fields)
                .all(|field| at_most_in(p, &resolve(field, *m, args, rows), bound, assumes, seen))
        }
        Ty::Array(elem, m) => le(*m) && at_most_in(p, &inherit(elem, *m), bound, assumes, seen),
        Ty::Fn(_, m) | Ty::Any(m) => le(*m),
        Ty::Ref(_) | Ty::Cont(_) => le(Mutability::Mut),
        // A token holds nothing.
        Ty::Reuse(_) => true,
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
    /// What the enclosing `case` arms, and the `new`s in scope, say about
    /// variables.
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
        let allowed = Allowed::func(f.mut_params.count, f.effect_params);
        for &(a, b) in &f.mut_params.assumes {
            for m in [a, b] {
                let ok = match m {
                    Mutability::Imm | Mutability::Mut | Mutability::ReadOnly => true,
                    Mutability::Param(i) => i >= 1 && i <= f.mut_params.count,
                    Mutability::Inherit => false,
                };
                if !ok {
                    self.err(format!(
                        "assumes {a}: {b}, but {m} isn't a mutability or one of its parameters"
                    ));
                }
            }
        }
        for (i, ty) in f.vars.iter().enumerate() {
            if let Err(message) = ty_ok(self.p, ty, allowed) {
                self.err(format!("type of %{i}: {message}"));
            }
        }
        if let Err(message) = row_ok(self.p, &f.row, f.effect_params) {
            self.err(message);
        }
        for ty in &f.results {
            if let Err(message) = ty_ok(self.p, ty, allowed) {
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
        let known = self.known.len();
        for l in &b.lets {
            self.source = l.source;
            if let Some(tys) = self.atoms(&l.args) {
                self.op(&l.op, &l.args, &tys, &l.binds);
            }
            for &v in &l.binds {
                self.bind(v);
            }
            // What `new` builds is known for as long as it's in scope,
            // and so is what a constant is.
            match (&l.op, &l.binds[..]) {
                (Op::New(c) | Op::Reuse(c), [v]) => self.known.push((*v, *c)),
                (Op::Const(id), [v]) => {
                    if let Some(ConstValue::New(c, _)) = const_value(self.p, *id) {
                        self.known.push((*v, *c));
                    }
                }
                _ => {}
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
            Tail::Unreachable | Tail::Trap => {}
        }
        self.known.truncate(known);
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
            Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 => {
                let mut seen = BTreeSet::new();
                for arm in arms {
                    match arm.tag {
                        // Tags are read as signed.
                        Tag::Int(n) if !fits_int(ty, n) => {
                            self.err(format!(
                                "case on an {ty} has a tag {n}, which isn't an {ty}"
                            ));
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
            Ty::Data(d, ..) => {
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
        let row = typing::op_row(self.p, op, tys);
        if !row.is_in(&self.f.row) {
            let does = if self.f.row.is_nothing() {
                "has no effects".to_string()
            } else {
                format!("only does {}", self.f.row)
            };
            let name = crate::text::print::op_name(op);
            self.err(format!("{name} may perform {row}, but the function {does}"));
        }
    }

    /// The types an op gives: see [`typing::op_results`].
    fn op_results(&self, op: &Op, args: &[Atom], tys: &[Ty], out: &[Ty]) -> Checked<Vec<Ty>> {
        typing::op_results(
            self.p,
            &self.f.mut_params,
            self.f.effect_params,
            op,
            args,
            tys,
            out,
            &self.known,
        )
    }
}

/// Objects are created, and copied, as `Imm` or `Mut`, not as a view. A
/// mutability parameter is one of the two.
fn object_mutability(m: Mutability) -> Checked<()> {
    match m {
        Mutability::Imm | Mutability::Mut | Mutability::Param(_) => Ok(()),
        _ => Err(format!("a new object is imm or mut, not {m}")),
    }
}

/// Checks the mutability arguments and rows a call or closure of `f`
/// gives, in a function with mutability parameters `caller` and `effects`
/// effect parameters, and gives `f`'s signature with them. `what` is
/// "call" or "closure", for messages.
#[allow(clippy::too_many_arguments)]
pub(crate) fn instantiate<P: Program + ?Sized>(
    p: &P,
    caller: &MutParams,
    effects: u32,
    what: &str,
    f: FuncId,
    margs: &[Mutability],
    rows: &[Row],
) -> Checked<Sig> {
    let callee_rows = p
        .func_effect_params(f)
        .ok_or_else(|| format!("unknown function {f}"))?;
    if rows.len() != callee_rows as usize {
        return Err(format!(
            "{what} of {f}: takes {callee_rows} rows, but gets {}",
            rows.len()
        ));
    }
    for row in rows {
        row_ok(p, row, effects).map_err(|m| format!("{what} of {f}: {m}"))?;
    }
    let sig = p
        .func_sig(f)
        .ok_or_else(|| format!("unknown function {f}"))?;
    let callee = p
        .func_mut_params(f)
        .ok_or_else(|| format!("unknown function {f}"))?;
    if margs.len() != callee.count as usize {
        return Err(format!(
            "{what} of {f}: takes {} mutability arguments, but gets {}",
            callee.count,
            margs.len()
        ));
    }
    for &m in margs {
        let ok = match m {
            Mutability::Imm | Mutability::Mut => true,
            Mutability::Param(i) => i >= 1 && i <= caller.count,
            Mutability::ReadOnly | Mutability::Inherit => false,
        };
        if !ok {
            return Err(format!(
                "{what} of {f}: the mutability argument {m} isn't imm, mut or a parameter"
            ));
        }
    }
    let pick = |m: Mutability| match m {
        Mutability::Param(i) => param_arg(margs, i).unwrap_or(m),
        m => m,
    };
    for &(a, b) in &callee.assumes {
        let (a2, b2) = (pick(a), pick(b));
        if !a2.at_most(b2, &caller.assumes) {
            return Err(format!(
                "{what} of {f}: {f} assumes {a}: {b}, so it needs {a2}: {b2}, which doesn't hold"
            ));
        }
    }
    Ok(subst_sig_rows(&subst_sig(&sig, margs), rows))
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
    let int = Ty::is_int;
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
            let size = |t: &Ty| match t {
                Ty::I8 => 8,
                Ty::I16 => 16,
                Ty::I64 | Ty::F64 => 64,
                _ => 32,
            };
            if !num(to) || int(&from) == int(to) || size(&from) != size(to) {
                return Err(format!("can't reinterpret a {from} as a {to}"));
            }
            to.clone()
        }
    };
    Ok(vec![ty])
}

/// Whether `n` is in the range of the integer type `ty`, read as signed.
fn fits_int(ty: &Ty, n: i64) -> bool {
    match ty {
        Ty::I8 => i8::try_from(n).is_ok(),
        Ty::I16 => i16::try_from(n).is_ok(),
        Ty::I32 => i32::try_from(n).is_ok(),
        _ => true,
    }
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
/// run-time check catches instead (see `docs/verify.md`). And it doesn't
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
        // Counting a continuation's references isn't using it.
        for l in b.lets.iter().filter(|l| !is_count_op(&l.op)) {
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
            Tail::Unreachable | Tail::Trap => Counts::new(),
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
        Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
    }
}

#[cfg(test)]
mod tests;

/// Whether an op only counts references, which isn't using what it counts.
fn is_count_op(op: &Op) -> bool {
    matches!(
        op,
        Op::Dup | Op::Drop | Op::IsUnique | Op::Free | Op::DecRef
    )
}
