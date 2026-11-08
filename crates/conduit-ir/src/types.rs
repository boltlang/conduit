//! What types become with arguments in place of their parameters, and
//! what they can hold: the type arithmetic that the verifier, the builder,
//! the evaluator, the passes and the backends share.

use std::collections::BTreeSet;

use crate::*;

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
pub(crate) fn param_arg(args: &[Mutability], i: u32) -> Option<Mutability> {
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
        // A token holds nothing, and Conduit sees nothing in a host value.
        Ty::Reuse(_) | Ty::Host => true,
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
    a.fields.len() == b.fields.len()
        && a.fields
            .iter()
            .zip(&b.fields)
            .all(|(x, y)| match (x.is_scalar(), y.is_scalar()) {
                (false, false) => true,
                (true, true) => x == y,
                _ => false,
            })
}
