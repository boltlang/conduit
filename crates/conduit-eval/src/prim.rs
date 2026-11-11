//! Scalar operations. See the crate docs for how they behave.

use conduit_ir::{Lit, Prim, Sign, Ty};

use crate::error::{ErrorKind, unverified};
use crate::value::Value;

pub(crate) fn eval(prim: &Prim, args: &[Value]) -> Result<Value, ErrorKind> {
    if args.contains(&Value::Unset) {
        return Err(crate::machine::undefined_use());
    }
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
        (Value::I8(x), Value::I8(y)) => int8(prim, x, y),
        (Value::I16(x), Value::I16(y)) => int16(prim, x, y),
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
            let bool = |b: bool| Ok(Value::I32(b as i32));
            let n = match prim {
                Prim::Add => x.wrapping_add(y),
                Prim::Sub => x.wrapping_sub(y),
                Prim::Mul => x.wrapping_mul(y),
                // Dividing by zero gives 0, and the remainder is then `x`.
                Prim::Div(_) | Prim::Rem(_) if y == 0 => match prim {
                    Prim::Div(_) => 0,
                    _ => x,
                },
                Prim::Div(Sign::Signed) => x.checked_div(y).ok_or(ErrorKind::IntegerOverflow)?,
                Prim::Div(Sign::Unsigned) => (ux / uy) as $s,
                Prim::Rem(Sign::Signed) => x.wrapping_rem(y),
                Prim::Rem(Sign::Unsigned) => (ux % uy) as $s,
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

int_binary!(int8, i8, u8, I8);
int_binary!(int16, i16, u16, I16);
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
                Prim::Min => wasm_min(x, y),
                Prim::Max => -wasm_min(-x, -y),
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

/// The smaller of two floats as Wasm's `min` gives it, unlike Rust's: NaN
/// if either is NaN, and `-0.0` is smaller than `0.0`.
fn wasm_min<F: Float>(x: F, y: F) -> F {
    if x.is_nan() || y.is_nan() {
        F::NAN
    } else if x == y {
        // Equal but different only for zeros: the negative one is smaller.
        if x.is_sign_negative() { x } else { y }
    } else if x < y {
        x
    } else {
        y
    }
}

trait Float: Copy + PartialOrd + std::ops::Neg<Output = Self> {
    const NAN: Self;
    fn is_nan(self) -> bool;
    fn is_sign_negative(self) -> bool;
}

macro_rules! float {
    ($f:ty) => {
        impl Float for $f {
            const NAN: Self = <$f>::NAN;
            fn is_nan(self) -> bool {
                <$f>::is_nan(self)
            }
            fn is_sign_negative(self) -> bool {
                <$f>::is_sign_negative(self)
            }
        }
    };
}

float!(f32);
float!(f64);

float_binary!(float32, f32, F32);
float_binary!(float64, f64, F64);

fn unary(prim: &Prim, a: Value) -> Result<Value, ErrorKind> {
    match prim {
        Prim::Neg => match a {
            Value::I8(x) => Ok(Value::I8(x.wrapping_neg())),
            Value::I16(x) => Ok(Value::I16(x.wrapping_neg())),
            Value::I32(x) => Ok(Value::I32(x.wrapping_neg())),
            Value::I64(x) => Ok(Value::I64(x.wrapping_neg())),
            Value::F32(x) => Ok(Value::F32(-x)),
            Value::F64(x) => Ok(Value::F64(-x)),
            Value::Obj(_) | Value::Host(_) | Value::Function(_) | Value::Null | Value::Unset => {
                Err(mismatch(prim))
            }
        },
        Prim::Abs | Prim::Sqrt => {
            let sqrt = matches!(prim, Prim::Sqrt);
            match a {
                Value::F32(x) => Ok(Value::F32(if sqrt { x.sqrt() } else { x.abs() })),
                Value::F64(x) => Ok(Value::F64(if sqrt { x.sqrt() } else { x.abs() })),
                _ => Err(mismatch(prim)),
            }
        }
        Prim::Convert(to, sign) => convert(a, to, *sign)?.ok_or_else(|| mismatch(prim)),
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
/// to make an integer result from a float. An integer result keeps the low
/// bits of the value. A float that is NaN or out of the integer's range
/// after truncating is an error, as in Wasm. `None` for types it doesn't
/// convert between.
fn convert(a: Value, to: &Ty, sign: Sign) -> Result<Option<Value>, ErrorKind> {
    let signed = sign == Sign::Signed;
    // An integer argument by value, read with `sign`.
    let int = |raw: i64, bits: u32| -> i128 {
        if signed {
            raw as i128
        } else {
            raw as i128 & ((1i128 << bits) - 1)
        }
    };
    let n = match a {
        Value::I8(x) => Some(int(x as i64, 8)),
        Value::I16(x) => Some(int(x as i64, 16)),
        Value::I32(x) => Some(int(x as i64, 32)),
        Value::I64(x) => Some(int(x, 64)),
        _ => None,
    };
    let float = match a {
        Value::F32(x) => Some(x as f64),
        Value::F64(x) => Some(x),
        _ => None,
    };
    let bits = match to {
        Ty::I8 => Some(8),
        Ty::I16 => Some(16),
        Ty::I32 => Some(32),
        Ty::I64 => Some(64),
        _ => None,
    };
    // An integer result with the low bits of `n`.
    let make = |n: i128| match to {
        Ty::I8 => Value::I8(n as i8),
        Ty::I16 => Value::I16(n as i16),
        Ty::I32 => Value::I32(n as i32),
        _ => Value::I64(n as i64),
    };
    Ok(Some(match (n, float, bits) {
        (Some(n), _, Some(_)) => make(n),
        (Some(n), _, None) => match to {
            Ty::F32 => Value::F32(n as f32),
            Ty::F64 => Value::F64(n as f64),
            _ => return Ok(None),
        },
        (_, Some(x), Some(bits)) => {
            // The range of the integer, as `low <= t < high`: powers of
            // two, so exact as floats.
            let (low, high) = if signed {
                (-(2f64.powi(bits - 1)), 2f64.powi(bits - 1))
            } else {
                (0.0, 2f64.powi(bits))
            };
            let t = x.trunc();
            if !(low <= t && t < high) {
                return Err(ErrorKind::InvalidConversion);
            }
            make(t as i128)
        }
        (_, Some(_), None) => match (a, to) {
            (Value::F32(x), Ty::F32) => Value::F32(x),
            (Value::F32(x), Ty::F64) => Value::F64(x as f64),
            (Value::F64(x), Ty::F32) => Value::F32(x as f32),
            (Value::F64(x), Ty::F64) => Value::F64(x),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    }))
}

/// What `prim` gives on `args`, as the evaluator computes it, or `None` if
/// it traps or the arguments don't fit. For constant folding.
pub fn fold(prim: &Prim, args: &[Lit]) -> Option<Lit> {
    let args: Vec<Value> = args.iter().map(|&a| a.into()).collect();
    eval(prim, &args).ok()?.lit()
}
