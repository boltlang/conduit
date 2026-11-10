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

use crate::types::*;
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
        // An extern's row says what its callers must be allowed to do; the
        // call itself performs nothing.
        if let Err(message) = sig_ok(m, &ext.sig, 0, 0)
            .and_then(|()| row_ok(m, &ext.sig.row, 0))
            .and_then(|()| abi_ok(ext))
        {
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
    // Each export is of a function, by a name no other export of its kind
    // has.
    let mut names = BTreeMap::new();
    for (&id, export) in &m.exports {
        if !m.funcs.contains_key(&id) {
            errors.push(decl_error(
                Item::Func(id),
                format!("{id} is exported, but isn't a function"),
            ));
        }
        if let Some(other) = names.insert((export.kind(), export.name()), id) {
            errors.push(decl_error(
                Item::Func(id),
                format!(
                    "{id} is exported as {} `{}`, as {other} is",
                    export.kind(),
                    export.name()
                ),
            ));
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
    let funcs: Vec<FuncId> = h.clauses().map(|(_, _, c)| c).chain([h.ret]).collect();
    for clause in &funcs {
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
    let mut seen = Vec::new();
    for (e, clauses) in &h.ops {
        let Some(effect) = p.effect(*e) else {
            err(format!("unknown effect {e}"));
            continue;
        };
        if seen.contains(e) {
            err(format!("handles {e} twice"));
            continue;
        }
        seen.push(*e);
        if clauses.len() != effect.ops.len() {
            err(format!(
                "has {} clauses for {e}, but {e} has {} operations",
                clauses.len(),
                effect.ops.len()
            ));
            continue;
        }
        for (i, (&clause, op)) in clauses.iter().zip(&effect.ops).enumerate() {
            let Some(sig) = p.func_sig(clause) else {
                err(format!("unknown clause {clause} for {e}.{i}"));
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
                    "clause {clause} for {e}.{i} has type {sig}, but should have {expected}"
                ));
            } else if !sig.row.is_in(&h.row) {
                err(format!(
                    "clause {clause} for {e}.{i} does {}, which isn't in the handler's row {}",
                    sig.row, h.row
                ));
            }
        }
    }
    if h.ops.is_empty() {
        err("handles no effects".into());
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
        (ConstValue::Closure(f), Ty::Fn(sig, _)) => {
            let fsig = p
                .func_sig(*f)
                .ok_or_else(|| format!("unknown function {f}"))?;
            if p.func_mut_params(*f).is_some_and(|mp| mp.count > 0)
                || p.func_effect_params(*f).is_some_and(|n| n > 0)
            {
                return Err(format!(
                    "{f} has mutability or effect parameters, which a constant closure can't give"
                ));
            }
            if fsig.params != sig.params || fsig.results != sig.results || !fsig.row.is_in(&sig.row)
            {
                return Err(format!("{f} has type {fsig}, not fn{sig}"));
            }
            Ok(())
        }
        (ConstValue::Closure(f), _) => Err(format!("a closure of {f} isn't a {ty}")),
        (ConstValue::Box(inner_ty, inner), Ty::Any(_)) => {
            ty_ok(p, inner_ty, Allowed::PLAIN)?;
            if !deep_imm(p, inner_ty) {
                return Err(format!(
                    "a constant box holds what's imm all the way down, but {inner_ty} isn't"
                ));
            }
            const_value_ok(p, inner, inner_ty).map_err(|m| format!("in box[{inner_ty}]: {m}"))
        }
        (ConstValue::Box(..), _) => Err(format!("a box isn't a {ty}")),
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
        Ty::Host | Ty::NullableHost => Ok(()),
        Ty::Reuse(c) => match p.data(c.data).and_then(|d| d.ctors.get(c.index as usize)) {
            Some(_) => Ok(()),
            None => Err(format!("unknown constructor {c}")),
        },
    }
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

/// Checks what an extern's ABI can pass: see [`Abi::Flat`].
fn abi_ok(ext: &Extern) -> Result<(), String> {
    let n = ext.sig.params.len();
    if !ext.addr.is_empty() && ext.addr.len() != n {
        return Err(format!(
            "has {} addr flags for {n} parameters",
            ext.addr.len()
        ));
    }
    match ext.abi {
        Abi::Conduit => match (0..n).find(|&i| ext.addr(i)) {
            Some(i) => Err(format!(
                "parameter {i} is #[addr], which only the flat ABI has"
            )),
            None => Ok(()),
        },
        Abi::Flat => {
            for (i, ty) in ext.sig.params.iter().enumerate() {
                let ok = match ty {
                    ty if ty.is_scalar() => !ext.addr(i),
                    Ty::Array(elem, _) if elem.is_scalar() => true,
                    Ty::Array(elem, _) => {
                        matches!(&**elem, Ty::Array(b, _) if **b == Ty::I8) && !ext.addr(i)
                    }
                    _ => false,
                };
                if !ok {
                    let what = if ext.addr(i) { "#[addr] " } else { "" };
                    return Err(format!(
                        "the flat ABI can't pass parameter {i}, a {what}{ty}: only scalars, \
                         arrays of scalars, #[addr] or not, and arrays of [i8]"
                    ));
                }
            }
            match ext.sig.results.iter().position(|ty| !ty.is_scalar()) {
                Some(i) => Err(format!(
                    "the flat ABI can't give result {i}, a {}: only scalars",
                    ext.sig.results[i]
                )),
                None => Ok(()),
            }
        }
    }
}

/// An effect operation's signature has no row: its effects are its
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

        let mut linear = Linear {
            f,
            labels: BTreeMap::new(),
            over: BTreeSet::new(),
        };
        let counts = linear.uses(&f.body);
        let mut over = linear.over;
        over.extend(counts.into_iter().filter(|&(_, n)| n > 1).map(|(v, _)| v));
        for var in over {
            self.source = None;
            self.err(format!("continuation {var} may be used more than once"));
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
        crate::deep(|| {
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
        })
    }

    fn join(&mut self, rec: bool, defs: &[JoinDef], body: &Block) {
        crate::deep(|| {
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
        })
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
        crate::deep(|| {
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
        })
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
        let row = op.row(self.p, tys);
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
    /// The continuations bound inside the function that may be used more
    /// than once, found where they're bound.
    over: BTreeSet<Var>,
}

impl Linear<'_> {
    /// Takes `vars`, which are bound here, out of `c`, which then only
    /// counts what's bound outside, noting those used more than once.
    fn bound(&mut self, c: &mut Counts, vars: &[Var]) {
        for v in vars {
            if c.remove(v).is_some_and(|n| n > 1) {
                self.over.insert(*v);
            }
        }
    }
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
        crate::deep(|| {
            let mut c = Counts::new();
            // Counting a continuation's references isn't using it.
            for l in b.lets.iter().filter(|l| !l.op.is_count()) {
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
            for l in &b.lets {
                self.bound(&mut c, &l.binds);
            }
            c
        })
    }

    /// What a join point's body uses, of variables bound outside it: those
    /// bound inside are new each time the join point is entered.
    fn def_uses(&mut self, def: &JoinDef) -> Counts {
        crate::deep(|| {
            let mut c = self.uses(&def.body);
            self.bound(&mut c, &def.params);
            c
        })
    }
}

#[cfg(test)]
mod tests;
