use crate::id::{ConstId, CtorId, EffectId, ExternId, FuncId, HandlerId};
use crate::row::Row;
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
    /// mutability parameters, and a row for each of its effect parameters.
    Call(FuncId, Vec<Mutability>, Vec<Row>),
    /// A call of a host function.
    CallExtern(ExternId),
    /// A closure of a lifted function, with an argument for each of its
    /// mutability parameters and a row for each of its effect parameters.
    /// Args: the captured values, which come first among the function's
    /// parameters.
    Closure(FuncId, Vec<Mutability>, Vec<Row>),
    /// Calls a closure. Args: the closure, then the arguments.
    Apply,
    /// Allocates an ADT value. Args: its fields.
    New(CtorId),
    /// A constant's value, which is the same object every time. Args:
    /// none. It binds a variable of the constant's type, or a supertype.
    Const(ConstId),
    /// A value of the type it binds that must not be used, like LLVM's
    /// `poison`: passing it on is fine, but using it, as an operand of an
    /// op that looks at it or a `case`, is undefined behavior. For code
    /// that has to give a value it knows won't be looked at. Args: none.
    Undefined,
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
    /// Allocates a `Mut` array whose elements aren't set yet. Reading one
    /// before it's set is undefined behavior. Args: the length.
    ArrayAlloc,
    /// Args: the array, then the index.
    ArrayGet,
    /// Writes to a `Mut` array; binds nothing. Args: the array, the index,
    /// then the value.
    ArraySet,
    /// Args: the array.
    ArrayLen,
    /// Copies elements from one array into a `Mut` one, as if through a
    /// temporary, so the two can be the same array and the ranges can
    /// overlap; binds nothing. Unset elements stay unset. Args: the
    /// destination, its start, the source, its start, then how many.
    ArrayCopy,
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
    /// Runs a closure under a handler, with a row for each of its effect
    /// parameters. Args: the handler's environment, then the closure.
    Handle(HandlerId, Vec<Row>),
    /// Runs operation `i` of the nearest enclosing handler of the effect.
    Perform(EffectId, u32),
    /// Continues a captured computation. Args: the continuation, then the
    /// values to resume with.
    Resume,
    /// Continues a captured computation by performing operation `i` of the
    /// effect where it was captured, instead of giving it values: what that
    /// gives is what the captured `perform` gives. Args: the continuation,
    /// then the operation's arguments.
    Discontinue(EffectId, u32),
    /// Adds 1 to an object's reference count; binds nothing. Args: the
    /// object. See "Reference counts" in `docs/ir.md`.
    Dup,
    /// Takes 1 from an object's reference count, and frees it when that
    /// reaches 0, which drops what it holds; binds nothing. Args: the
    /// object. Using the object after it's freed, or dropping it when its
    /// count is already 0, is undefined behavior.
    Drop,
    /// Whether an object's count is 1, as an `i32`: 1 if it is, 0 if not,
    /// or if the object isn't counted. Args: the object. What specializing
    /// `Drop` branches on.
    IsUnique,
    /// Frees an object whose count is 1, without dropping what it holds,
    /// which the code has already taken; binds nothing. Args: the object.
    /// Undefined behavior if its count isn't 1.
    Free,
    /// Takes 1 from the count of an object that `IsUnique` showed is
    /// shared, so it's never freed; binds nothing. Args: the object.
    /// Undefined behavior if its count is 1.
    DecRef,
    /// `Drop` of a data object known to be built by a constructor `C`,
    /// which gives a reuse token of type `Reuse(C)`: the object's memory,
    /// if its count was 1, after dropping what it held, or none. Args: the
    /// object.
    DropReuse,
    /// The memory of a unique data object known to be built by `C`, whose
    /// fields the code has taken already, as a reuse token of type
    /// `Reuse(C)`: what `DropReuse` gives when the object is unique, without
    /// dropping the fields. Args: the object. Undefined behavior if its
    /// count isn't 1. What specializing `DropReuse` makes.
    ReuseToken,
    /// A null `NullableHost`. Args: none.
    HostNull,
    /// Whether a `NullableHost` is null, as an `i32`: 1 if it is, 0 if
    /// not. Args: the value.
    HostIsNull,
    /// A `NullableHost` that isn't null as a `Host`, the same value; it
    /// traps if it's null. Args: the value, whose reference it takes.
    HostUnwrap,
    /// A host function that applies a closure: in Wasm, a JavaScript
    /// function, which the host can call as any other, like an event
    /// listener. The host holds the closure for as long as it holds the
    /// function. The closure's parameters and results must be ones the
    /// host can give and take: scalars, `Host` and `NullableHost`. It runs
    /// with no handlers, as an export does. Args: the closure, whose
    /// reference it takes.
    HostFunction,
    /// `New`, in the memory of a reuse token if it has some. Args: the
    /// token, of a constructor of the same shape (see
    /// [`same_shape`](crate::types::same_shape)), then the fields.
    Reuse(CtorId),
}

/// What an op may do: to memory, and whether it may stop the program
/// instead of giving results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Effects {
    pub memory: Memory,
    /// Whether it may trap. An op that may trap can't be removed, nor run
    /// where it didn't run before. Two of them with the same arguments can
    /// still be one: if the first didn't trap, the second won't.
    pub may_trap: bool,
}

/// What an op may do to memory that existed before it, from least to
/// most. Making new objects isn't counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Memory {
    /// Nothing: it only computes its results.
    None,
    /// May read.
    Reads,
    /// May write.
    Writes,
    /// May do anything: it runs code, which may also perform effects and
    /// not come back.
    Unknown,
}

impl Effects {
    /// Only computes its results.
    pub const PURE: Effects = Effects::new(Memory::None, false);

    pub const fn new(memory: Memory, may_trap: bool) -> Effects {
        Effects { memory, may_trap }
    }

    /// Whether the op can be removed if its results are unused: it doesn't
    /// write, and can't trap.
    pub fn is_removable(&self) -> bool {
        self.memory <= Memory::Reads && !self.may_trap
    }
}

impl Op {
    /// Whether the op only counts references, which reference counting
    /// adds: `Dup`, `Drop`, `IsUnique`, `Free`, `DecRef`, `DropReuse` and
    /// `ReuseToken`. Counting an object's references isn't using it.
    pub fn is_count(&self) -> bool {
        matches!(
            self,
            Op::Dup
                | Op::Drop
                | Op::IsUnique
                | Op::Free
                | Op::DecRef
                | Op::DropReuse
                | Op::ReuseToken
        )
    }

    /// What an op may perform, given its arguments' types: an operation's
    /// effect, a callee's, extern's, closure's or continuation's row, or a
    /// handler's. An extern's row only limits who may call it: the host
    /// performs nothing. A
    /// `discontinue` also performs its operation where the continuation was
    /// captured.
    pub fn row<P: crate::Program + ?Sized>(&self, p: &P, tys: &[Ty]) -> Row {
        let first = || match tys.first() {
            Some(Ty::Fn(sig, _) | Ty::Cont(sig)) => sig.row.clone(),
            _ => Row::NOTHING,
        };
        match self {
            Op::Perform(e, _) => Row::one(*e),
            Op::Call(f, _, rows) => p
                .func_sig(*f)
                .map_or(Row::NOTHING, |sig| sig.row.subst(rows)),
            Op::CallExtern(e) => p.extern_fn(*e).map_or(Row::NOTHING, |x| x.sig.row.clone()),
            Op::Apply | Op::Resume => first(),
            Op::Discontinue(e, _) => first().union(&Row::one(*e)),
            Op::Handle(h, rows) => p
                .handler(*h)
                .map_or(Row::NOTHING, |decl| decl.row.subst(rows)),
            _ => Row::NOTHING,
        }
    }

    pub fn effects(&self) -> Effects {
        use Memory::*;
        let (memory, may_trap) = match self {
            Op::Prim(p) => (None, p.may_trap()),
            Op::Closure(..) | Op::Box(_) | Op::New(_) | Op::ArrayNew | Op::RefNew => (None, false),
            Op::Const(_) | Op::Undefined => (None, false),
            Op::HostNull | Op::HostIsNull => (None, false),
            Op::HostUnwrap => (None, true),
            Op::HostFunction => (None, false),
            // Its length may be negative or too big.
            Op::ArrayFill | Op::ArrayAlloc => (None, true),
            Op::Get(..) | Op::ArrayLen | Op::ArrayClone | Op::RefGet => (Reads, false),
            // It changes objects that nothing else can reach any more.
            Op::ArrayFreeze => (Reads, false),
            // Bounds checks and failed casts trap.
            Op::ArrayGet | Op::Unbox(_) => (Reads, true),
            Op::Set(..) | Op::RefSet => (Writes, false),
            // They write the count, and `Drop` may free.
            Op::Dup | Op::Drop | Op::Free | Op::DecRef | Op::DropReuse | Op::ReuseToken => {
                (Writes, false)
            }
            // It may write into memory that's no object's any more, and it
            // must not be removed, since its token would be lost.
            Op::Reuse(_) => (Writes, false),
            Op::IsUnique => (Reads, false),
            Op::ArraySet | Op::ArrayCopy => (Writes, true),
            Op::Call(..) | Op::CallExtern(_) | Op::Apply => (Unknown, true),
            Op::Handle(..) | Op::Perform(..) | Op::Resume | Op::Discontinue(..) => (Unknown, true),
        };
        Effects::new(memory, may_trap)
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
    /// Signed integer division traps when the result doesn't fit, as for
    /// the smallest value divided by -1, and float-to-integer conversions
    /// trap when out of range. This only knows the op, not its operand
    /// types, so it's conservative: a signed float division doesn't trap,
    /// nor does a conversion between integers. Division and remainder by
    /// zero don't trap: `x / 0` is 0 and `x % 0` is `x`.
    pub fn may_trap(&self) -> bool {
        matches!(self, Prim::Div(Sign::Signed) | Prim::Convert(..))
    }
}
