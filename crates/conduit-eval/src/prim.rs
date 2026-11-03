//! Scalar operations. See the crate docs for how they behave.

use conduit_ir::{Prim, Sign, Ty};

use crate::error::{ErrorKind, unverified};
use crate::value::Value;

pub(crate) fn eval(prim: &Prim, args: &[Value]) -> Result<Value, ErrorKind> {
    match *args {
        [a] => unary(prim, a),
        [a, b] => binary(prim, a, b),
        _ => Err(unverified(format!(
            "{prim:?} with {} arguments",
            args.len()
        ))),
    }
}

fn mismatch(prim: &Prim) -> ErrorKind {
    unverified(format!("{prim:?} on values of the wrong types"))
}

fn binary(prim: &Prim, a: Value, b: Value) -> Result<Value, ErrorKind> {
    match (a, b) {
        (Value::I32(x), Value::I32(y)) => int32(prim, x, y),
        (Value::I64(x), Value::I64(y)) => int64(prim, x, y),
        (Value::F32(x), Value::F32(y)) => float32(prim, x, y),
        (Value::F64(x), Value::F64(y)) => float64(prim, x, y),
        _ => Err(mismatch(prim)),
    }
}

macro_rules! int_binary {
    ($name:ident, $s:ty, $u:ty, $variant:ident) => {
        fn $name(prim: &Prim, x: $s, y: $s) -> Result<Value, ErrorKind> {
            let (ux, uy) = (x as $u, y as $u);
            let nonzero = || {
                if y == 0 {
                    Err(ErrorKind::DivideByZero)
                } else {
                    Ok(())
                }
            };
            let bool = |b: bool| Ok(Value::I32(b as i32));
            let n = match prim {
                Prim::Add => x.wrapping_add(y),
                Prim::Sub => x.wrapping_sub(y),
                Prim::Mul => x.wrapping_mul(y),
                Prim::Div(Sign::Signed) => {
                    nonzero()?;
                    x.checked_div(y).ok_or(ErrorKind::IntegerOverflow)?
                }
                Prim::Div(Sign::Unsigned) => {
                    nonzero()?;
                    (ux / uy) as $s
                }
                Prim::Rem(Sign::Signed) => {
                    nonzero()?;
                    x.wrapping_rem(y)
                }
                Prim::Rem(Sign::Unsigned) => {
                    nonzero()?;
                    (ux % uy) as $s
                }
                Prim::And => x & y,
                Prim::Or => x | y,
                Prim::Xor => x ^ y,
                Prim::Shl => x.wrapping_shl(y as u32),
                Prim::Shr(Sign::Signed) => x.wrapping_shr(y as u32),
                Prim::Shr(Sign::Unsigned) => ux.wrapping_shr(y as u32) as $s,
                Prim::Eq => return bool(x == y),
                Prim::Ne => return bool(x != y),
                Prim::Lt(Sign::Signed) => return bool(x < y),
                Prim::Lt(Sign::Unsigned) => return bool(ux < uy),
                Prim::Le(Sign::Signed) => return bool(x <= y),
                Prim::Le(Sign::Unsigned) => return bool(ux <= uy),
                Prim::Gt(Sign::Signed) => return bool(x > y),
                Prim::Gt(Sign::Unsigned) => return bool(ux > uy),
                Prim::Ge(Sign::Signed) => return bool(x >= y),
                Prim::Ge(Sign::Unsigned) => return bool(ux >= uy),
                _ => return Err(mismatch(prim)),
            };
            Ok(Value::$variant(n))
        }
    };
}

int_binary!(int32, i32, u32, I32);
int_binary!(int64, i64, u64, I64);

macro_rules! float_binary {
    ($name:ident, $f:ty, $variant:ident) => {
        fn $name(prim: &Prim, x: $f, y: $f) -> Result<Value, ErrorKind> {
            let bool = |b: bool| Ok(Value::I32(b as i32));
            let n = match prim {
                Prim::Add => x + y,
                Prim::Sub => x - y,
                Prim::Mul => x * y,
                Prim::Div(_) => x / y,
                Prim::Min => x.min(y),
                Prim::Max => x.max(y),
                Prim::Eq => return bool(x == y),
                Prim::Ne => return bool(x != y),
                Prim::Lt(_) => return bool(x < y),
                Prim::Le(_) => return bool(x <= y),
                Prim::Gt(_) => return bool(x > y),
                Prim::Ge(_) => return bool(x >= y),
                _ => return Err(mismatch(prim)),
            };
            Ok(Value::$variant(n))
        }
    };
}

float_binary!(float32, f32, F32);
float_binary!(float64, f64, F64);

fn unary(prim: &Prim, a: Value) -> Result<Value, ErrorKind> {
    match prim {
        Prim::Neg => match a {
            Value::I32(x) => Ok(Value::I32(x.wrapping_neg())),
            Value::I64(x) => Ok(Value::I64(x.wrapping_neg())),
            Value::F32(x) => Ok(Value::F32(-x)),
            Value::F64(x) => Ok(Value::F64(-x)),
            Value::Obj(_) => Err(mismatch(prim)),
        },
        Prim::Abs | Prim::Sqrt => {
            let sqrt = matches!(prim, Prim::Sqrt);
            match a {
                Value::F32(x) => Ok(Value::F32(if sqrt { x.sqrt() } else { x.abs() })),
                Value::F64(x) => Ok(Value::F64(if sqrt { x.sqrt() } else { x.abs() })),
                _ => Err(mismatch(prim)),
            }
        }
        Prim::Convert(to, sign) => convert(a, to, *sign).ok_or_else(|| mismatch(prim)),
        Prim::Reinterpret(to) => match (a, to) {
            (Value::I32(x), Ty::F32) => Ok(Value::F32(f32::from_bits(x as u32))),
            (Value::F32(x), Ty::I32) => Ok(Value::I32(x.to_bits() as i32)),
            (Value::I64(x), Ty::F64) => Ok(Value::F64(f64::from_bits(x as u64))),
            (Value::F64(x), Ty::I64) => Ok(Value::I64(x.to_bits() as i64)),
            _ => Err(mismatch(prim)),
        },
        _ => Err(mismatch(prim)),
    }
}

/// Converts by value. `sign` says how to read an integer argument, or how
/// to make an integer result from a float.
fn convert(a: Value, to: &Ty, sign: Sign) -> Option<Value> {
    let signed = sign == Sign::Signed;
    Some(match (a, to) {
        (Value::I32(x), Ty::I32) => Value::I32(x),
        (Value::I32(x), Ty::I64) => Value::I64(if signed { x as i64 } else { x as u32 as i64 }),
        (Value::I32(x), Ty::F32) => Value::F32(if signed { x as f32 } else { x as u32 as f32 }),
        (Value::I32(x), Ty::F64) => Value::F64(if signed { x as f64 } else { x as u32 as f64 }),
        (Value::I64(x), Ty::I32) => Value::I32(x as i32),
        (Value::I64(x), Ty::I64) => Value::I64(x),
        (Value::I64(x), Ty::F32) => Value::F32(if signed { x as f32 } else { x as u64 as f32 }),
        (Value::I64(x), Ty::F64) => Value::F64(if signed { x as f64 } else { x as u64 as f64 }),
        (Value::F32(x), Ty::I32) => Value::I32(if signed { x as i32 } else { x as u32 as i32 }),
        (Value::F32(x), Ty::I64) => Value::I64(if signed { x as i64 } else { x as u64 as i64 }),
        (Value::F32(x), Ty::F32) => Value::F32(x),
        (Value::F32(x), Ty::F64) => Value::F64(x as f64),
        (Value::F64(x), Ty::I32) => Value::I32(if signed { x as i32 } else { x as u32 as i32 }),
        (Value::F64(x), Ty::I64) => Value::I64(if signed { x as i64 } else { x as u64 as i64 }),
        (Value::F64(x), Ty::F32) => Value::F32(x as f32),
        (Value::F64(x), Ty::F64) => Value::F64(x),
        _ => return None,
    })
}
