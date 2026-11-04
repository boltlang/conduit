use crate::id::{CtorId, EffectId, ExternId, FuncId, HandlerId};
use crate::ty::{Mutability, Ty};

/// Everything that isn't control flow. Passes that move code around don't
/// look inside an `Op`: they ask it for its [`Effects`].
///
/// The types of an op's results are those of the variables it binds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Op {
    /// Arithmetic, comparisons and conversions on scalars.
    Prim(Prim),
    /// A direct call, with an argument for each of the function's
    /// mutability parameters.
    Call(FuncId, Vec<Mutability>),
    /// A call of a host function.
    CallExtern(ExternId),
    /// A closure of a lifted function, with an argument for each of its
    /// mutability parameters. Args: the captured values, which come first
    /// among the function's parameters.
    Closure(FuncId, Vec<Mutability>),
    /// Calls a closure. Args: the closure, then the arguments.
    Apply,
    /// Allocates an ADT value. Args: its fields.
    New(CtorId),
    /// Reads a field. Args: the object.
    Get(CtorId, u32),
    /// Writes a field of a `Mut` object; binds nothing. Args: the object,
    /// then the value.
    Set(CtorId, u32),
    /// Allocates an array. Args: its elements.
    ArrayNew,
    /// Allocates an array of one value repeated. Args: the length, then
    /// the value.
    ArrayFill,
    /// Args: the array, then the index.
    ArrayGet,
    /// Writes to a `Mut` array; binds nothing. Args: the array, the index,
    /// then the value.
    ArraySet,
    /// Args: the array.
    ArrayLen,
    /// A new array with the same elements, which it shares, of the
    /// mutability the variable it binds says. Args: the array.
    ArrayClone,
    /// The array itself as `Imm`: what an `ArrayClone` to `Imm` gives, made
    /// by turning the array `Imm` in place. Args: the array.
    ///
    /// Undefined behavior if anything other than the result can still reach
    /// the array afterwards. Optimizations make it from an `ArrayClone` that
    /// they show is the only way left to the array.
    ArrayFreeze,
    /// Allocates a ref. Args: the initial value.
    RefNew,
    /// Args: the ref.
    RefGet,
    /// Binds nothing. Args: the ref, then the new value.
    RefSet,
    /// A value of this type as an `Any`. Args: the value.
    Box(Ty),
    /// An `Any` as a value of this type. Args: the `Any`.
    Unbox(Ty),
    /// Runs a closure under a handler. Args: the handler's environment,
    /// then the closure.
    Handle(HandlerId),
    /// Runs operation `i` of the nearest enclosing handler of the effect.
    Perform(EffectId, u32),
    /// Continues a captured computation. Args: the continuation, then the
    /// values to resume with.
    Resume,
}

/// What an op may do, from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effects {
    /// Nothing but compute its results: it can be removed if they're unused.
    Pure,
    /// May read memory or allocate, but writes nothing that existed before.
    Reads,
    /// May write memory.
    Writes,
    /// May do anything: call code, perform effects, trap.
    Unknown,
}

impl Op {
    pub fn effects(&self) -> Effects {
        match self {
            Op::Prim(p) if p.may_trap() => Effects::Unknown,
            Op::Prim(_) | Op::Closure(..) | Op::Box(_) => Effects::Pure,
            Op::New(_) | Op::ArrayNew | Op::RefNew => Effects::Pure,
            Op::Get(..) | Op::ArrayLen | Op::ArrayClone | Op::RefGet => Effects::Reads,
            // It changes objects that nothing else can reach any more.
            Op::ArrayFreeze => Effects::Reads,
            Op::Set(..) | Op::RefSet => Effects::Writes,
            // Bounds checks and failed casts trap.
            Op::ArrayFill | Op::ArrayGet | Op::ArraySet | Op::Unbox(_) => Effects::Unknown,
            Op::Call(..) | Op::CallExtern(_) | Op::Apply => Effects::Unknown,
            Op::Handle(_) | Op::Perform(..) | Op::Resume => Effects::Unknown,
        }
    }
}

/// Signedness, for the integer ops where it matters. Ignored for floats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sign {
    Signed,
    Unsigned,
}

/// A scalar op. The operand types come from the arguments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Prim {
    Add,
    Sub,
    Mul,
    Div(Sign),
    Rem(Sign),
    Neg,
    And,
    Or,
    Xor,
    Shl,
    Shr(Sign),
    Eq,
    Ne,
    Lt(Sign),
    Le(Sign),
    Gt(Sign),
    Ge(Sign),
    Abs,
    Sqrt,
    Min,
    Max,
    /// To another scalar type, by value.
    Convert(Ty, Sign),
    /// To a scalar type of the same size, by bits.
    Reinterpret(Ty),
}

impl Prim {
    /// Integer division and remainder trap on zero, and float-to-integer
    /// conversions trap when out of range. The backend only knows which
    /// from the operand types, so this is conservative.
    pub fn may_trap(&self) -> bool {
        matches!(self, Prim::Div(_) | Prim::Rem(_) | Prim::Convert(..))
    }
}
