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
}

impl Object<'_> {
    /// The values the object holds, other than a continuation's frames.
    pub fn values(&self) -> &[Value] {
        match self {
            Object::Data { fields, .. } => fields,
            Object::Array { elems, .. } => elems,
            Object::Ref(v) | Object::Any { value: v, .. } => std::slice::from_ref(v),
            Object::Closure { captures, .. } => captures,
            Object::Cont(_) => &[],
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

/// An object's reference count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// How many references own the object. It starts at 1.
    Live(u32),
    /// Its count reached 0. Without [`counting`](crate::Machine::counting),
    /// nothing is freed.
    Freed,
    /// Never counted: a constant's object.
    Immortal,
}

/// What [`Machine::leaks`](crate::Machine::leaks) found. Each list is
/// sorted by object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leaks {
    /// Objects with more counts than references, and how many more:
    /// something that owned a reference never dropped it.
    pub undropped: Vec<(ObjId, u32)>,
    /// Objects with fewer counts than references, and how many fewer:
    /// something dropped a reference it didn't own.
    pub overdropped: Vec<(ObjId, u32)>,
    /// Freed objects that a live object or a root still refers to.
    pub dangling: Vec<ObjId>,
    /// Garbage whose counts are all right: only other garbage refers to
    /// it, so it's in a cycle, or reachable only from one. Counting can't
    /// free it, which is expected until there is a cycle collector.
    pub cycles: Vec<ObjId>,
}

impl Leaks {
    /// Whether every count is right and nothing is left but cycles.
    pub fn counts_ok(&self) -> bool {
        self.undropped.is_empty() && self.overdropped.is_empty() && self.dangling.is_empty()
    }

    /// Whether everything the run made that the roots don't own was freed.
    pub fn is_empty(&self) -> bool {
        self.counts_ok() && self.cycles.is_empty()
    }
}

impl std::fmt::Display for Leaks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let objs = |list: &mut dyn Iterator<Item = String>| list.collect::<Vec<_>>().join(", ");
        let mut parts = Vec::new();
        if !self.undropped.is_empty() {
            let list = objs(
                &mut self
                    .undropped
                    .iter()
                    .map(|(o, n)| format!("#{} (+{n})", o.0)),
            );
            parts.push(format!("undropped: {list}"));
        }
        if !self.overdropped.is_empty() {
            let list = objs(
                &mut self
                    .overdropped
                    .iter()
                    .map(|(o, n)| format!("#{} (-{n})", o.0)),
            );
            parts.push(format!("dropped too often: {list}"));
        }
        if !self.dangling.is_empty() {
            let list = objs(&mut self.dangling.iter().map(|o| format!("#{}", o.0)));
            parts.push(format!("freed but referred to: {list}"));
        }
        if !self.cycles.is_empty() {
            let list = objs(&mut self.cycles.iter().map(|o| format!("#{}", o.0)));
            parts.push(format!("kept alive by cycles: {list}"));
        }
        if parts.is_empty() {
            write!(f, "no leaks")
        } else {
            write!(f, "{}", parts.join("; "))
        }
    }
}
