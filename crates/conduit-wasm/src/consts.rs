//! Constants, as immutable globals.

use std::collections::BTreeSet;

use conduit_ir::verify::{const_refs, elem_ty, resolve};
use conduit_ir::{ConstId, ConstValue, Lit, Module, Ty};
use wasm_encoder::{ConstExpr, Ieee32, Ieee64, Instruction as I};

use crate::Cx;

/// The module's constants, each after those it refers to.
pub(crate) fn order(m: &Module) -> Vec<ConstId> {
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    for &start in m.consts.keys() {
        if !seen.insert(start) {
            continue;
        }
        // A depth-first walk that adds a constant once those it refers to
        // are in.
        let mut stack = vec![(start, refs(m, start))];
        while let Some((id, refs)) = stack.last_mut() {
            match refs.pop() {
                Some(next) => {
                    if m.consts.contains_key(&next) && seen.insert(next) {
                        let next_refs = self::refs(m, next);
                        stack.push((next, next_refs));
                    }
                }
                None => {
                    order.push(*id);
                    stack.pop();
                }
            }
        }
    }
    order
}

fn refs(m: &Module, id: ConstId) -> Vec<ConstId> {
    m.consts
        .get(&id)
        .map_or(Vec::new(), |c| const_refs(&c.value))
}

/// The initializer of the global of a constant of type `ty`.
pub(crate) fn init(cx: &mut Cx<'_>, value: &ConstValue, ty: &Ty) -> Result<ConstExpr, String> {
    let mut code = Vec::new();
    value_code(cx, value, ty, &mut code)?;
    Ok(ConstExpr::extended(code))
}

/// The code that makes `value`, of type `ty`, from constant instructions.
fn value_code(
    cx: &mut Cx<'_>,
    value: &ConstValue,
    ty: &Ty,
    code: &mut Vec<I<'static>>,
) -> Result<(), String> {
    match (value, ty) {
        (ConstValue::Lit(lit), _) => code.push(match *lit {
            Lit::I8(n) => I::I32Const(n.into()),
            Lit::I16(n) => I::I32Const(n.into()),
            Lit::I32(n) => I::I32Const(n),
            Lit::I64(n) => I::I64Const(n),
            Lit::F32(bits) => I::F32Const(Ieee32::new(bits)),
            Lit::F64(bits) => I::F64Const(Ieee64::new(bits)),
        }),
        (ConstValue::Array(elems), Ty::Array(raw, _)) => {
            let array = cx.types.array(raw)?;
            let elem = elem_ty(ty).unwrap();
            for e in elems {
                value_code(cx, e, &elem, code)?;
            }
            code.push(I::ArrayNewFixed {
                array_type_index: array,
                array_size: elems.len() as u32,
            });
        }
        (ConstValue::New(c, fields), Ty::Data(_, m, args)) => {
            let (idx, first) = cx.types.ctor(*c)?;
            if first == 1 {
                code.push(I::I32Const(c.index as i32));
            }
            let decls =
                cx.m.data
                    .get(&c.data)
                    .and_then(|d| d.ctors.get(c.index as usize))
                    .ok_or_else(|| format!("unknown constructor {c}"))?;
            for (field, value) in decls.fields.iter().zip(fields) {
                value_code(cx, value, &resolve(field, *m, args), code)?;
            }
            code.push(I::StructNew(idx));
        }
        (ConstValue::Const(id), _) => code.push(I::GlobalGet(cx.const_global(*id)?)),
        (value, ty) => return Err(format!("{value:?} isn't a {ty}")),
    }
    Ok(())
}
