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
    /// A host value, as a host function sees one: its number, which the
    /// host chose when it gave the value. In the machine, a value of type
    /// `host` is an [`Object::Host`] instead, counted like other objects.
    Host(u64),
    /// A host function made by `host_function`, as a host function sees
    /// one: its number among the closures the host holds, which
    /// [`Machine::call_function`](crate::Machine::call_function) calls and
    /// [`Machine::release_function`](crate::Machine::release_function)
    /// lets go of.
    Function(u32),
    /// A null `nullable host`, which isn't an object, so it isn't counted.
    /// A host function sees it, and gives it, as itself.
    Null,
    /// What `Undefined` gives, or an element of an array that
    /// `ArrayAlloc` made and nothing has set yet. Passing it on is fine,
    /// but using it is undefined behavior, which stops the machine. An
    /// unset element stops `ArrayGet` already.
    Unset,
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
    /// A host value, by the number its host gave it.
    Host(u64),
    /// A host value that's a host function made by `host_function`, by
    /// its number among the closures the host holds.
    HostFunction(u32),
}

impl Object<'_> {
    /// The values the object holds, other than a continuation's frames.
    pub fn values(&self) -> &[Value] {
        match self {
            Object::Data { fields, .. } => fields,
            Object::Array { elems, .. } => elems,
            Object::Ref(v) | Object::Any { value: v, .. } => std::slice::from_ref(v),
            Object::Closure { captures, .. } => captures,
            Object::Cont(_) | Object::Host(_) | Object::HostFunction(_) => &[],
        }
    }
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
