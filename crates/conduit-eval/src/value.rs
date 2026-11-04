use conduit_ir::{CtorId, FuncId, Lit, Mutability, Ty};

use crate::machine::Frame;

/// A value: a scalar, or a reference to an object in the machine's heap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Obj(ObjId),
}

impl From<Lit> for Value {
    fn from(lit: Lit) -> Value {
        match lit {
            Lit::I8(n) => Value::I8(n),
            Lit::I16(n) => Value::I16(n),
            Lit::I32(n) => Value::I32(n),
            Lit::I64(n) => Value::I64(n),
            Lit::F32(bits) => Value::F32(f32::from_bits(bits)),
            Lit::F64(bits) => Value::F64(f64::from_bits(bits)),
        }
    }
}

/// An object in the heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjId(pub u32);

/// What the heap holds.
#[derive(Debug)]
pub enum Object<'p> {
    Data {
        ctor: CtorId,
        fields: Vec<Value>,
        is_mut: bool,
    },
    Array {
        elems: Vec<Value>,
        is_mut: bool,
    },
    Ref(Value),
    Closure {
        func: FuncId,
        /// The function's mutability arguments, each `Imm` or `Mut`.
        margs: Vec<Mutability>,
        captures: Vec<Value>,
        is_mut: bool,
    },
    /// A boxed value, with the type it was boxed at, so that `Unbox` can
    /// tell a wrong type.
    Any {
        ty: Ty,
        value: Value,
    },
    Cont(Continuation<'p>),
}

/// A captured computation: the frames from a handler up to a `perform`.
#[derive(Debug)]
pub struct Continuation<'p> {
    /// `None` once it has been resumed.
    pub(crate) frames: Option<Vec<Frame<'p>>>,
}

impl Continuation<'_> {
    pub fn is_used(&self) -> bool {
        self.frames.is_none()
    }
}
