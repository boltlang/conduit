//! The types each op gives, shared by the verifier and the builder (see
//! [`crate::build`]), so that they agree.

use super::{
    Allowed, Checked, at_most, elem_ty, fits, inherit, instantiate, object_mutability,
    prim_results, resolve, sub, ty_ok,
};
use crate::*;

/// The types an op gives. For ops whose result type the IR doesn't
/// spell out, like `New`'s mutability, that's the types of the
/// variables it binds, `out`. `mp` is the parameters of the function the
/// op is in.
pub(crate) fn op_results<P: Program + ?Sized>(
    p: &P,
    mp: &MutParams,
    op: &Op,
    args: &[Atom],
    tys: &[Ty],
    out: &[Ty],
    known: &[(Var, CtorId)],
) -> Checked<Vec<Ty>> {
    match op {
        Op::Prim(prim) => prim_results(prim, tys),
        Op::Call(f, margs) => {
            let sig = instantiate(p, mp, "call", *f, margs)?;
            fits(tys, &sig.params).map_err(|m| format!("call of {f}: {m}"))?;
            Ok(sig.results)
        }
        Op::CallExtern(e) => {
            let ext = p
                .extern_fn(*e)
                .ok_or_else(|| format!("unknown extern {e}"))?;
            fits(tys, &ext.sig.params).map_err(|m| format!("call of {e}: {m}"))?;
            Ok(ext.sig.results.clone())
        }
        Op::Closure(f, margs) => {
            let sig = instantiate(p, mp, "closure", *f, margs)?;
            let [Ty::Fn(fn_sig, m)] = out else {
                return Err(format!("closure of {f} must bind one closure"));
            };
            object_mutability(*m)?;
            let captures = sig.params.len().checked_sub(fn_sig.params.len());
            let ok = captures.is_some_and(|n| {
                sig.params[n..] == fn_sig.params[..] && sig.results == fn_sig.results
            });
            let Some(n) = captures.filter(|_| ok) else {
                return Err(format!(
                    "{f} has type {sig}, which doesn't end with {fn_sig}"
                ));
            };
            fits(tys, &sig.params[..n]).map_err(|m| format!("captures of {f}: {m}"))?;
            if let Some(ty) = tys.iter().find(|ty| !at_most(p, ty, *m, &mp.assumes)) {
                return Err(format!("{} {m} closure can't capture a {ty}", article(*m)));
            }
            Ok(out.to_vec())
        }
        Op::Apply => {
            let Some((Ty::Fn(sig, _), rest)) = tys.split_first() else {
                return Err("apply needs a closure as its first argument".into());
            };
            fits(rest, &sig.params).map_err(|m| format!("apply: {m}"))?;
            Ok(sig.results.clone())
        }
        Op::New(c) => {
            let [Ty::Data(d, m, margs)] = out else {
                return Err(format!("new {c} must bind one value of its data type"));
            };
            if *d != c.data {
                return Err(format!("new {c} binds a {}", out[0]));
            }
            object_mutability(*m)?;
            let fields = fields(p, *c)?;
            if fields.len() != tys.len() {
                return Err(format!(
                    "{c} has {} fields, but new gives {}",
                    fields.len(),
                    tys.len()
                ));
            }
            for (i, (ty, field)) in tys.iter().zip(fields).enumerate() {
                let field = resolve(field, *m, margs);
                if !at_most(p, &field, *m, &mp.assumes) {
                    return Err(format!(
                        "{} {m} {c} can't hold field {i} of type {field}",
                        article(*m)
                    ));
                }
                if !sub(ty, &field) {
                    return Err(format!(
                        "field {i} of {c} has type {field}, but gets a {ty}"
                    ));
                }
            }
            Ok(out.to_vec())
        }
        Op::Get(c, i) => {
            let [Ty::Data(d, m, margs)] = tys else {
                return Err(format!("get of {c} needs one object"));
            };
            field_access(p, known, c, *d, &args[0])?;
            Ok(vec![field(p, *c, *i, *m, margs)?])
        }
        Op::Set(c, i) => {
            let [Ty::Data(d, m, margs), value] = tys else {
                return Err(format!("set of {c} needs an object and a value"));
            };
            if *m != Mutability::Mut {
                return Err(format!("can't set a field of a {}", tys[0]));
            }
            field_access(p, known, c, *d, &args[0])?;
            let field = field(p, *c, *i, Mutability::Mut, margs)?;
            if !sub(value, &field) {
                return Err(format!(
                    "field {i} of {c} has type {field}, but gets a {value}"
                ));
            }
            Ok(vec![])
        }
        Op::ArrayNew => {
            let elem = new_array(p, mp, out)?;
            for ty in tys {
                if !sub(ty, &elem) {
                    return Err(format!("array of {elem} gets an element of type {ty}"));
                }
            }
            Ok(out.to_vec())
        }
        Op::ArrayFill => {
            let elem = new_array(p, mp, out)?;
            let [Ty::I32, ty] = tys else {
                return Err("array fill needs an i32 length and a value".into());
            };
            if !sub(ty, &elem) {
                return Err(format!("array of {elem} gets an element of type {ty}"));
            }
            Ok(out.to_vec())
        }
        Op::ArrayGet => match tys {
            [array @ Ty::Array(..), Ty::I32] => Ok(vec![elem_ty(array).unwrap()]),
            _ => Err("array get needs an array and an i32 index".into()),
        },
        Op::ArraySet => match tys {
            [array @ Ty::Array(_, Mutability::Mut), Ty::I32, ty] => {
                let elem = elem_ty(array).unwrap();
                if sub(ty, &elem) {
                    Ok(vec![])
                } else {
                    Err(format!("array of {elem} gets an element of type {ty}"))
                }
            }
            [array @ Ty::Array(..), Ty::I32, _] => {
                Err(format!("can't set an element of a {array}"))
            }
            _ => Err("array set needs an array, an i32 index and a value".into()),
        },
        Op::ArrayLen => match tys {
            [Ty::Array(..)] => Ok(vec![Ty::I32]),
            _ => Err("array len needs an array".into()),
        },
        Op::RefNew => match (tys, out) {
            ([ty], [Ty::Ref(inner)]) if sub(ty, inner) => Ok(out.to_vec()),
            ([ty], [Ty::Ref(inner)]) => Err(format!("ref of {inner} gets a {ty}")),
            _ => Err("ref new needs one value and binds one ref".into()),
        },
        Op::RefGet => match tys {
            [Ty::Ref(inner)] => Ok(vec![(**inner).clone()]),
            _ => Err("ref get needs a ref".into()),
        },
        Op::RefSet => match tys {
            [Ty::Ref(inner), ty] if sub(ty, inner) => Ok(vec![]),
            [Ty::Ref(inner), ty] => Err(format!("ref of {inner} gets a {ty}")),
            _ => Err("ref set needs a ref and a value".into()),
        },
        Op::Box(ty) => {
            ty_ok(p, ty, Allowed::func(mp.count))?;
            match tys {
                [arg] if sub(arg, ty) => {}
                [arg] => return Err(format!("box of {ty} gets a {arg}")),
                _ => return Err("box needs one value".into()),
            }
            let m = if at_most(p, ty, Mutability::Imm, &mp.assumes) {
                Mutability::Imm
            } else {
                Mutability::Mut
            };
            Ok(vec![Ty::Any(m)])
        }
        Op::Unbox(ty) => {
            ty_ok(p, ty, Allowed::func(mp.count))?;
            match tys {
                [Ty::Any(_)] => Ok(vec![ty.clone()]),
                _ => Err("unbox needs one any".into()),
            }
        }
        Op::ArrayClone => array_clone(p, mp, tys, out, false),
        Op::ArrayFreeze => array_clone(p, mp, tys, out, true),
        Op::Handle(h) => {
            let decl = p
                .handler(*h)
                .ok_or_else(|| format!("unknown handler {h}"))?;
            let ret = p
                .func_sig(decl.ret)
                .ok_or_else(|| format!("unknown function {}", decl.ret))?;
            let n = decl.env.len();
            let Some((Ty::Fn(body, _), env)) = tys.split_last() else {
                return Err(format!("handle {h} needs its environment, then a closure"));
            };
            fits(env, &decl.env).map_err(|m| format!("environment of {h}: {m}"))?;
            let returns = ret.params.get(n..).unwrap_or_default();
            if !body.params.is_empty() || body.results != returns {
                return Err(format!(
                    "the body of handle {h} has type fn{body}, but should have fn() -> ({})",
                    returns
                        .iter()
                        .map(Ty::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok(ret.results)
        }
        Op::Perform(e, i) => {
            let effect = p.effect(*e).ok_or_else(|| format!("unknown effect {e}"))?;
            let sig = effect
                .ops
                .get(*i as usize)
                .ok_or_else(|| format!("{e} has no operation {i}"))?;
            fits(tys, &sig.params).map_err(|m| format!("perform {e}.{i}: {m}"))?;
            Ok(sig.results.clone())
        }
        Op::Resume => {
            let Some((Ty::Cont(sig), rest)) = tys.split_first() else {
                return Err("resume needs a continuation as its first argument".into());
            };
            fits(rest, &sig.params).map_err(|m| format!("resume: {m}"))?;
            Ok(sig.results.clone())
        }
    }
}

fn fields<P: Program + ?Sized>(p: &P, c: CtorId) -> Checked<&[Ty]> {
    let decl = p
        .data(c.data)
        .ok_or_else(|| format!("unknown data type {}", c.data))?;
    let ctor = decl
        .ctors
        .get(c.index as usize)
        .ok_or_else(|| format!("unknown constructor {c}"))?;
    Ok(&ctor.fields)
}

/// The type of field `i` of an object of constructor `c`, mutability `m`
/// and mutability arguments `args`.
fn field<P: Program + ?Sized>(
    p: &P,
    c: CtorId,
    i: u32,
    m: Mutability,
    args: &[Mutability],
) -> Checked<Ty> {
    let field = fields(p, c)?
        .get(i as usize)
        .ok_or_else(|| format!("{c} has no field {i}"))?;
    Ok(resolve(field, m, args))
}

/// The types an `ArrayClone` gives, or with `freeze`, an `ArrayFreeze`: an array
/// of the type it binds, `out`, whose elements are those of the array in
/// `tys`. A `ArrayFreeze` gives an `Imm` array, which a builder that doesn't
/// say gets with the same elements.
fn array_clone<P: Program + ?Sized>(
    p: &P,
    mp: &MutParams,
    tys: &[Ty],
    out: &[Ty],
    freeze: bool,
) -> Checked<Vec<Ty>> {
    let what = if freeze {
        "array freeze"
    } else {
        "array clone"
    };
    let [from @ Ty::Array(..)] = tys else {
        return Err(format!("{what} needs one array"));
    };
    let have = elem_ty(from).unwrap();
    let default;
    let out = match out {
        [] if freeze => {
            default = [Ty::Array(Box::new(have.clone()), Mutability::Imm)];
            &default[..]
        }
        out => out,
    };
    if freeze && !matches!(out, [Ty::Array(_, Mutability::Imm)]) {
        return Err("array freeze must bind one imm array".into());
    }
    let elem = new_array(p, mp, out)?;
    if !sub(&have, &elem) {
        return Err(format!(
            "{what} of a {from} can't give a {}: its elements are {have}",
            out[0]
        ));
    }
    Ok(out.to_vec())
}

/// Checks that `obj`, of data type `d`, is known to be built by `c`:
/// `d` has one constructor, `obj` was bound by `new c`, or an enclosing
/// `case` arm says so.
fn field_access<P: Program + ?Sized>(
    p: &P,
    known: &[(Var, CtorId)],
    c: &CtorId,
    d: DataId,
    obj: &Atom,
) -> Checked<()> {
    if c.data != d {
        return Err(format!("{c} isn't a constructor of {d}"));
    }
    let ctors = p.data(d).map_or(0, |decl| decl.ctors.len());
    let known = match obj {
        Atom::Var(v) => known.contains(&(*v, *c)),
        Atom::Lit(_) => false,
    };
    if ctors == 1 || known {
        Ok(())
    } else {
        Err(format!(
            "{obj} isn't known to be built by {c} here, so its fields can't be accessed"
        ))
    }
}

/// The element type of an array that an op creates and binds.
fn new_array<P: Program + ?Sized>(p: &P, mp: &MutParams, out: &[Ty]) -> Checked<Ty> {
    let [Ty::Array(elem, m)] = out else {
        return Err("an array op must bind one array".into());
    };
    object_mutability(*m)?;
    let elem = inherit(elem, *m);
    if !at_most(p, &elem, *m, &mp.assumes) {
        return Err(format!(
            "{} {m} array can't hold elements of type {elem}",
            article(*m)
        ));
    }
    Ok(elem)
}

/// "an imm array", "a mut array", "a 'm1 array".
fn article(m: Mutability) -> &'static str {
    match m {
        Mutability::Imm | Mutability::Inherit => "an",
        _ => "a",
    }
}
