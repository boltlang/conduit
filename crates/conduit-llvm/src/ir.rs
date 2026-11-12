//! A small model of LLVM IR, with only what the backend uses: the backend
//! builds a [`Module`] of these, and printing it gives the text that
//! `clang` and `llc` read. [`FnBuilder`] builds a function an instruction
//! at a time.

use std::fmt::{self, Display, Formatter};

/// A type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Void,
    I1,
    I8,
    I16,
    I32,
    I64,
    Float,
    Double,
    Ptr,
    Struct(Vec<Type>),
    /// A struct without padding: `<{ .. }>`.
    Packed(Vec<Type>),
    Array(u64, Box<Type>),
}

impl Type {
    /// `v`, as a value of this type.
    pub fn val(&self, v: impl Into<Value>) -> Typed {
        Typed(self.clone(), v.into())
    }
}

/// A value: a local or a global by its name, without the `%` or `@`, or a
/// constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Local(String),
    Global(String),
    Int(i64),
    /// The bits of a double: LLVM writes a `float` as the double with the
    /// same value, too.
    Float(u64),
    Null,
    Undef,
    /// `zeroinitializer`.
    Zero,
    Struct(Vec<Typed>),
    Packed(Vec<Typed>),
    /// A string of bytes: `c".."`.
    Bytes(Vec<u8>),
}

impl Value {
    pub fn global(name: impl Into<String>) -> Value {
        Value::Global(name.into())
    }
}

impl From<&Value> for Value {
    fn from(v: &Value) -> Value {
        v.clone()
    }
}

macro_rules! int_values {
    ($($t:ty)*) => {$(
        impl From<$t> for Value {
            fn from(n: $t) -> Value {
                Value::Int(n as i64)
            }
        }
    )*};
}

int_values!(i32 i64 u32 u64 usize);

/// A value with its type, as an operand or an argument is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed(pub Type, pub Value);

/// A block's label, without the `%`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallConv {
    C,
    /// `tailcc`, which guarantees that a call marked `tail` right before a
    /// `ret` is a tail call.
    Tail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    SDiv,
    UDiv,
    SRem,
    URem,
    And,
    Or,
    Xor,
    Shl,
    LShr,
    AShr,
    FAdd,
    FSub,
    FMul,
    FDiv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntPred {
    Eq,
    Ne,
    Slt,
    Sle,
    Sgt,
    Sge,
    Ult,
    Ule,
    Ugt,
    Uge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatPred {
    Oeq,
    Une,
    Olt,
    Ole,
    Ogt,
    Oge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastOp {
    Trunc,
    ZExt,
    SExt,
    SIToFP,
    UIToFP,
    FPToSI,
    FPToUI,
    FPExt,
    FPTrunc,
    Bitcast,
}

/// A call. With `variadic`, the callee is variadic, and these are the
/// types of the parameters before the `...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub tail: bool,
    pub cc: CallConv,
    pub ret: Type,
    pub variadic: Option<Vec<Type>>,
    pub callee: Value,
    pub args: Vec<Typed>,
}

impl Call {
    pub fn new(cc: CallConv, ret: Type, callee: impl Into<Value>, args: Vec<Typed>) -> Call {
        Call {
            tail: false,
            cc,
            ret,
            variadic: None,
            callee: callee.into(),
            args,
        }
    }

    /// A call of a `tailcc` function.
    pub fn tailcc(ret: Type, callee: impl Into<Value>, args: Vec<Typed>) -> Call {
        Call::new(CallConv::Tail, ret, callee, args)
    }

    /// A call of a C function.
    pub fn c(ret: Type, callee: impl Into<Value>, args: Vec<Typed>) -> Call {
        Call::new(CallConv::C, ret, callee, args)
    }

    /// The same call, marked `tail`.
    pub fn tail(mut self) -> Call {
        self.tail = true;
        self
    }
}

/// An instruction that isn't a terminator. `dest` is the name of the
/// local it defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inst {
    /// With `align`, the alignment in bytes the slot has; without, the
    /// type's own.
    Alloca {
        dest: String,
        ty: Type,
        align: Option<u32>,
    },
    /// With `align`, the alignment in bytes the address is known to have;
    /// without, the type's own.
    Load {
        dest: String,
        ty: Type,
        ptr: Value,
        align: Option<u32>,
    },
    Store {
        value: Typed,
        ptr: Value,
        align: Option<u32>,
    },
    Gep {
        dest: String,
        elem: Type,
        ptr: Value,
        indices: Vec<Typed>,
    },
    Bin {
        dest: String,
        op: BinOp,
        ty: Type,
        lhs: Value,
        rhs: Value,
    },
    FNeg {
        dest: String,
        value: Typed,
    },
    ICmp {
        dest: String,
        pred: IntPred,
        ty: Type,
        lhs: Value,
        rhs: Value,
    },
    FCmp {
        dest: String,
        pred: FloatPred,
        ty: Type,
        lhs: Value,
        rhs: Value,
    },
    Select {
        dest: String,
        cond: Value,
        then: Typed,
        other: Typed,
    },
    Cast {
        dest: String,
        op: CastOp,
        value: Typed,
        to: Type,
    },
    Call {
        dest: Option<String>,
        call: Call,
    },
    ExtractValue {
        dest: String,
        agg: Typed,
        index: u32,
    },
    InsertValue {
        dest: String,
        agg: Typed,
        elem: Typed,
        index: u32,
    },
    /// The first instruction of a block an `invoke` unwinds to, as a
    /// cleanup: a `{ ptr, i32 }` of the exception and its selector.
    LandingPad {
        dest: String,
    },
}

/// What a landing pad gives.
pub fn landing_pad_ty() -> Type {
    Type::Struct(vec![Type::Ptr, Type::I32])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Ret(Option<Typed>),
    /// A call that goes on at `normal`, or, if it unwinds, at `unwind`,
    /// which starts with a landing pad.
    Invoke {
        dest: Option<String>,
        call: Call,
        normal: Label,
        unwind: Label,
    },
    /// Goes on unwinding with what a landing pad gave.
    Resume(Value),
    Br(Label),
    CondBr {
        cond: Value,
        then: Label,
        other: Label,
    },
    Switch {
        value: Typed,
        default: Label,
        cases: Vec<(i64, Label)>,
    },
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub label: Label,
    pub insts: Vec<Inst>,
    /// Each instruction's debug location, the metadata node it's `!dbg`,
    /// if it has one.
    pub locs: Vec<Option<u32>>,
    pub term: Terminator,
    pub term_loc: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub cc: CallConv,
    pub ret: Type,
    /// Each parameter's type and name.
    pub params: Vec<(Type, String)>,
    pub blocks: Vec<Block>,
    /// Its attributes, like `cold`.
    pub attrs: Vec<&'static str>,
    /// Its personality function, if it has landing pads.
    pub personality: Option<Value>,
    /// Its `DISubprogram`, the metadata node it's `!dbg`, if it has debug
    /// info.
    pub dbg: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declare {
    pub name: String,
    pub ret: Type,
    pub params: Vec<Type>,
    pub variadic: bool,
    /// Function attributes, such as `noreturn`.
    pub attrs: Vec<&'static str>,
}

impl Declare {
    pub fn new(ret: Type, name: impl Into<String>, params: Vec<Type>) -> Declare {
        Declare {
            name: name.into(),
            ret,
            params,
            variadic: false,
            attrs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Global {
    pub name: String,
    /// `private unnamed_addr`: only this module sees it, and its address
    /// doesn't matter.
    pub private: bool,
    pub constant: bool,
    pub ty: Type,
    pub init: Value,
    pub align: Option<u32>,
}

impl Global {
    /// A private constant.
    pub fn constant(name: impl Into<String>, ty: Type, init: Value) -> Global {
        Global {
            name: name.into(),
            private: true,
            constant: true,
            ty,
            init,
            align: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Comment(String),
    Declare(Declare),
    Global(Global),
    Function(Function),
    /// LLVM IR written by hand, as is.
    Text(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Module {
    pub items: Vec<Item>,
    /// Lines of metadata, like `!3 = !DILocation(..)`, which go after the
    /// items.
    pub metadata: Vec<String>,
}

impl Module {
    pub fn push(&mut self, item: impl Into<Item>) {
        self.items.push(item.into());
    }
}

impl From<Declare> for Item {
    fn from(d: Declare) -> Item {
        Item::Declare(d)
    }
}

impl From<Global> for Item {
    fn from(g: Global) -> Item {
        Item::Global(g)
    }
}

impl From<Function> for Item {
    fn from(f: Function) -> Item {
        Item::Function(f)
    }
}

/// Builds a function: instructions go at the end of the current block,
/// until a terminator ends it, and [`start`](FnBuilder::start) starts the
/// next. `alloca`s all go at the start of the entry block, for `mem2reg`.
pub struct FnBuilder {
    func: Function,
    allocas: Vec<Inst>,
    current: Option<(Label, Vec<Inst>, Vec<Option<u32>>)>,
    temps: u32,
    labels: u32,
    /// The debug location of what's built next.
    loc: Option<u32>,
}

impl FnBuilder {
    /// A function with an empty `entry` block, the current one.
    pub fn new(
        cc: CallConv,
        ret: Type,
        name: impl Into<String>,
        params: Vec<(Type, String)>,
    ) -> FnBuilder {
        FnBuilder {
            func: Function {
                name: name.into(),
                cc,
                ret,
                params,
                blocks: Vec::new(),
                attrs: Vec::new(),
                personality: None,
                dbg: None,
            },
            allocas: Vec::new(),
            current: Some((Label("entry".into()), Vec::new(), Vec::new())),
            temps: 0,
            labels: 0,
            loc: None,
        }
    }

    /// Parameter `i`.
    pub fn param(&self, i: usize) -> Value {
        Value::Local(self.func.params[i].1.clone())
    }

    pub fn ret_ty(&self) -> &Type {
        &self.func.ret
    }

    /// The function, once its last block is terminated.
    /// Gives the function an attribute, like `cold`.
    pub fn attr(&mut self, attr: &'static str) {
        self.func.attrs.push(attr);
    }

    /// Gives the function its `DISubprogram`, metadata node `node`.
    pub fn subprogram(&mut self, node: u32) {
        self.func.dbg = Some(node);
    }

    /// The debug location, metadata node `node`, of what's built from now
    /// on, or none.
    pub fn set_loc(&mut self, node: Option<u32>) {
        self.loc = node;
    }

    /// Gives the function a personality, which its landing pads need.
    pub fn personality(&mut self, f: impl Into<Value>) {
        self.func.personality = Some(f.into());
    }

    pub fn finish(mut self) -> Function {
        assert!(
            self.current.is_none(),
            "{}: the last block isn't terminated",
            self.func.name
        );
        if let Some(entry) = self.func.blocks.first_mut() {
            let n = self.allocas.len();
            entry.insts.splice(0..0, self.allocas);
            entry.locs.splice(0..0, std::iter::repeat_n(None, n));
        }
        self.func
    }

    fn temp(&mut self) -> String {
        self.temps += 1;
        format!("t{}", self.temps)
    }

    /// A label for a new block, which [`start`](FnBuilder::start) starts.
    pub fn new_block(&mut self) -> Label {
        self.labels += 1;
        Label(format!("b{}", self.labels))
    }

    /// Starts the block `label`, once the current one is terminated.
    pub fn start(&mut self, label: &Label) {
        assert!(
            self.current.is_none(),
            "{}: starting {} before the block before it is terminated",
            self.func.name,
            label.0
        );
        self.current = Some((label.clone(), Vec::new(), Vec::new()));
    }

    fn push(&mut self, inst: Inst) {
        match &mut self.current {
            Some((_, insts, locs)) => {
                insts.push(inst);
                locs.push(self.loc);
            }
            None => panic!("{}: an instruction after a terminator", self.func.name),
        }
    }

    fn def(&mut self, make: impl FnOnce(String) -> Inst) -> Value {
        let dest = self.temp();
        self.push(make(dest.clone()));
        Value::Local(dest)
    }

    pub fn terminate(&mut self, term: Terminator) {
        let Some((label, insts, locs)) = self.current.take() else {
            panic!("{}: a terminator after a terminator", self.func.name);
        };
        self.func.blocks.push(Block {
            label,
            insts,
            locs,
            term,
            term_loc: self.loc,
        });
    }

    /// A slot for a value of type `ty`, in the entry block.
    pub fn alloca(&mut self, ty: Type) -> Value {
        let dest = self.temp();
        self.named_alloca(ty, dest)
    }

    /// A slot for a value of type `ty`, in the entry block, named `name`.
    pub fn named_alloca(&mut self, ty: Type, name: impl Into<String>) -> Value {
        let dest = name.into();
        self.allocas.push(Inst::Alloca {
            dest: dest.clone(),
            ty,
            align: None,
        });
        Value::Local(dest)
    }

    /// A slot for a value of type `ty` aligned to `align` bytes, in the
    /// entry block.
    pub fn aligned_alloca(&mut self, ty: Type, align: u32) -> Value {
        let dest = self.temp();
        self.allocas.push(Inst::Alloca {
            dest: dest.clone(),
            ty,
            align: Some(align),
        });
        Value::Local(dest)
    }

    pub fn load(&mut self, ty: Type, ptr: impl Into<Value>) -> Value {
        let ptr = ptr.into();
        self.def(|dest| Inst::Load {
            dest,
            ty,
            ptr,
            align: None,
        })
    }

    /// A load from an address that may have any alignment.
    pub fn load_unaligned(&mut self, ty: Type, ptr: impl Into<Value>) -> Value {
        let ptr = ptr.into();
        self.def(|dest| Inst::Load {
            dest,
            ty,
            ptr,
            align: Some(1),
        })
    }

    pub fn store(&mut self, ty: Type, value: impl Into<Value>, ptr: impl Into<Value>) {
        self.push(Inst::Store {
            value: Typed(ty, value.into()),
            ptr: ptr.into(),
            align: None,
        });
    }

    /// A store to an address that may have any alignment.
    pub fn store_unaligned(&mut self, ty: Type, value: impl Into<Value>, ptr: impl Into<Value>) {
        self.push(Inst::Store {
            value: Typed(ty, value.into()),
            ptr: ptr.into(),
            align: Some(1),
        });
    }

    pub fn gep(&mut self, elem: Type, ptr: impl Into<Value>, indices: Vec<Typed>) -> Value {
        let ptr = ptr.into();
        self.def(|dest| Inst::Gep {
            dest,
            elem,
            ptr,
            indices,
        })
    }

    /// The address `offset` bytes, an `i64`, past `ptr`.
    pub fn offset(&mut self, ptr: impl Into<Value>, offset: impl Into<Value>) -> Value {
        self.gep(Type::I8, ptr, vec![Type::I64.val(offset)])
    }

    /// The value of type `ty` at `offset` bytes past `ptr`.
    pub fn load_at(&mut self, ty: Type, ptr: impl Into<Value>, offset: impl Into<Value>) -> Value {
        let q = self.offset(ptr, offset);
        self.load(ty, q)
    }

    /// Stores `value`, of type `ty`, at `offset` bytes past `ptr`.
    pub fn store_at(
        &mut self,
        ty: Type,
        value: impl Into<Value>,
        ptr: impl Into<Value>,
        offset: impl Into<Value>,
    ) {
        let q = self.offset(ptr, offset);
        self.store(ty, value, q);
    }

    pub fn bin(
        &mut self,
        op: BinOp,
        ty: Type,
        lhs: impl Into<Value>,
        rhs: impl Into<Value>,
    ) -> Value {
        let (lhs, rhs) = (lhs.into(), rhs.into());
        self.def(|dest| Inst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        })
    }

    pub fn fneg(&mut self, ty: Type, value: impl Into<Value>) -> Value {
        let value = Typed(ty, value.into());
        self.def(|dest| Inst::FNeg { dest, value })
    }

    pub fn icmp(
        &mut self,
        pred: IntPred,
        ty: Type,
        lhs: impl Into<Value>,
        rhs: impl Into<Value>,
    ) -> Value {
        let (lhs, rhs) = (lhs.into(), rhs.into());
        self.def(|dest| Inst::ICmp {
            dest,
            pred,
            ty,
            lhs,
            rhs,
        })
    }

    pub fn fcmp(
        &mut self,
        pred: FloatPred,
        ty: Type,
        lhs: impl Into<Value>,
        rhs: impl Into<Value>,
    ) -> Value {
        let (lhs, rhs) = (lhs.into(), rhs.into());
        self.def(|dest| Inst::FCmp {
            dest,
            pred,
            ty,
            lhs,
            rhs,
        })
    }

    pub fn select(
        &mut self,
        cond: impl Into<Value>,
        ty: Type,
        then: impl Into<Value>,
        other: impl Into<Value>,
    ) -> Value {
        let cond = cond.into();
        let (then, other) = (ty.val(then), ty.val(other));
        self.def(|dest| Inst::Select {
            dest,
            cond,
            then,
            other,
        })
    }

    pub fn cast(&mut self, op: CastOp, from: Type, value: impl Into<Value>, to: Type) -> Value {
        let value = Typed(from, value.into());
        self.def(|dest| Inst::Cast {
            dest,
            op,
            value,
            to,
        })
    }

    /// A call, and what it gives, unless it gives `void`.
    pub fn call(&mut self, call: Call) -> Option<Value> {
        if call.ret == Type::Void {
            self.push(Inst::Call { dest: None, call });
            None
        } else {
            Some(self.def(|dest| Inst::Call {
                dest: Some(dest),
                call,
            }))
        }
    }

    /// Invokes `call`, which goes on in a new block, the current one
    /// after, or unwinds to `unwind`, and gives what the call gives,
    /// unless it gives `void`.
    pub fn invoke(&mut self, call: Call, unwind: &Label) -> Option<Value> {
        let dest = (call.ret != Type::Void).then(|| self.temp());
        let normal = self.new_block();
        self.terminate(Terminator::Invoke {
            dest: dest.clone(),
            call,
            normal: normal.clone(),
            unwind: unwind.clone(),
        });
        self.start(&normal);
        dest.map(Value::Local)
    }

    /// A cleanup landing pad, the first instruction of a block an
    /// `invoke` unwinds to.
    pub fn landing_pad(&mut self) -> Value {
        self.def(|dest| Inst::LandingPad { dest })
    }

    pub fn resume(&mut self, landed: impl Into<Value>) {
        self.terminate(Terminator::Resume(landed.into()));
    }

    /// A call that gives a value, and the value.
    pub fn call_value(&mut self, call: Call) -> Value {
        self.call(call).expect("a call that gives a value")
    }

    pub fn extract_value(&mut self, ty: Type, agg: impl Into<Value>, index: u32) -> Value {
        let agg = Typed(ty, agg.into());
        self.def(|dest| Inst::ExtractValue { dest, agg, index })
    }

    pub fn insert_value(
        &mut self,
        ty: Type,
        agg: impl Into<Value>,
        elem: Typed,
        index: u32,
    ) -> Value {
        let agg = Typed(ty, agg.into());
        self.def(|dest| Inst::InsertValue {
            dest,
            agg,
            elem,
            index,
        })
    }

    /// A call, then a return of what it gives.
    pub fn return_call(&mut self, call: Call) {
        let ret = call.ret.clone();
        let r = self.call(call);
        self.ret(r.map(|r| ret.val(r)));
    }

    pub fn ret(&mut self, value: Option<Typed>) {
        self.terminate(Terminator::Ret(value));
    }

    pub fn ret_void(&mut self) {
        self.ret(None);
    }

    pub fn br(&mut self, to: &Label) {
        self.terminate(Terminator::Br(to.clone()));
    }

    pub fn cond_br(&mut self, cond: impl Into<Value>, then: &Label, other: &Label) {
        self.terminate(Terminator::CondBr {
            cond: cond.into(),
            then: then.clone(),
            other: other.clone(),
        });
    }

    pub fn switch(&mut self, value: Typed, default: &Label, cases: Vec<(i64, Label)>) {
        self.terminate(Terminator::Switch {
            value,
            default: default.clone(),
            cases,
        });
    }

    pub fn unreachable(&mut self) {
        self.terminate(Terminator::Unreachable);
    }
}

/// Writes `items` separated by commas.
struct List<'a, T>(&'a [T]);

impl<T: Display> Display for List<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, x) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{x}")?;
        }
        Ok(())
    }
}

impl Display for Type {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Type::Void => f.write_str("void"),
            Type::I1 => f.write_str("i1"),
            Type::I8 => f.write_str("i8"),
            Type::I16 => f.write_str("i16"),
            Type::I32 => f.write_str("i32"),
            Type::I64 => f.write_str("i64"),
            Type::Float => f.write_str("float"),
            Type::Double => f.write_str("double"),
            Type::Ptr => f.write_str("ptr"),
            Type::Struct(tys) => write!(f, "{{ {} }}", List(tys)),
            Type::Packed(tys) => write!(f, "<{{ {} }}>", List(tys)),
            Type::Array(n, t) => write!(f, "[{n} x {t}]"),
        }
    }
}

impl Display for Value {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Value::Local(name) => write!(f, "%{name}"),
            Value::Global(name) => write!(f, "@{name}"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(bits) => write!(f, "0x{bits:016X}"),
            Value::Null => f.write_str("null"),
            Value::Undef => f.write_str("undef"),
            Value::Zero => f.write_str("zeroinitializer"),
            Value::Struct(fields) => write!(f, "{{ {} }}", List(fields)),
            Value::Packed(fields) => write!(f, "<{{ {} }}>", List(fields)),
            Value::Bytes(bytes) => {
                f.write_str("c\"")?;
                for &b in bytes {
                    if b.is_ascii_graphic() && b != b'"' && b != b'\\' || b == b' ' {
                        write!(f, "{}", b as char)?;
                    } else {
                        write!(f, "\\{b:02X}")?;
                    }
                }
                f.write_str("\"")
            }
        }
    }
}

impl Display for Typed {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.0, self.1)
    }
}

impl Display for Label {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "label %{}", self.0)
    }
}

impl Display for CallConv {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            CallConv::C => Ok(()),
            CallConv::Tail => f.write_str("tailcc "),
        }
    }
}

impl Display for BinOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::SDiv => "sdiv",
            BinOp::UDiv => "udiv",
            BinOp::SRem => "srem",
            BinOp::URem => "urem",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Xor => "xor",
            BinOp::Shl => "shl",
            BinOp::LShr => "lshr",
            BinOp::AShr => "ashr",
            BinOp::FAdd => "fadd",
            BinOp::FSub => "fsub",
            BinOp::FMul => "fmul",
            BinOp::FDiv => "fdiv",
        })
    }
}

impl Display for IntPred {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IntPred::Eq => "eq",
            IntPred::Ne => "ne",
            IntPred::Slt => "slt",
            IntPred::Sle => "sle",
            IntPred::Sgt => "sgt",
            IntPred::Sge => "sge",
            IntPred::Ult => "ult",
            IntPred::Ule => "ule",
            IntPred::Ugt => "ugt",
            IntPred::Uge => "uge",
        })
    }
}

impl Display for FloatPred {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FloatPred::Oeq => "oeq",
            FloatPred::Une => "une",
            FloatPred::Olt => "olt",
            FloatPred::Ole => "ole",
            FloatPred::Ogt => "ogt",
            FloatPred::Oge => "oge",
        })
    }
}

impl Display for CastOp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CastOp::Trunc => "trunc",
            CastOp::ZExt => "zext",
            CastOp::SExt => "sext",
            CastOp::SIToFP => "sitofp",
            CastOp::UIToFP => "uitofp",
            CastOp::FPToSI => "fptosi",
            CastOp::FPToUI => "fptoui",
            CastOp::FPExt => "fpext",
            CastOp::FPTrunc => "fptrunc",
            CastOp::Bitcast => "bitcast",
        })
    }
}

impl Display for Call {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.tail {
            f.write_str("tail ")?;
        }
        write!(f, "call {}{}", self.cc, self.ret)?;
        if let Some(fixed) = &self.variadic {
            let sep = if fixed.is_empty() { "" } else { ", " };
            write!(f, " ({}{sep}...)", List(fixed))?;
        }
        write!(f, " {}({})", self.callee, List(&self.args))
    }
}

impl Display for Inst {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Inst::Alloca { dest, ty, align } => {
                write!(f, "%{dest} = alloca {ty}")?;
                align.map_or(Ok(()), |a| write!(f, ", align {a}"))
            }
            Inst::Load {
                dest,
                ty,
                ptr,
                align,
            } => {
                write!(f, "%{dest} = load {ty}, ptr {ptr}")?;
                align.map_or(Ok(()), |a| write!(f, ", align {a}"))
            }
            Inst::Store { value, ptr, align } => {
                write!(f, "store {value}, ptr {ptr}")?;
                align.map_or(Ok(()), |a| write!(f, ", align {a}"))
            }
            Inst::Gep {
                dest,
                elem,
                ptr,
                indices,
            } => {
                write!(f, "%{dest} = getelementptr {elem}, ptr {ptr}")?;
                for i in indices {
                    write!(f, ", {i}")?;
                }
                Ok(())
            }
            Inst::Bin {
                dest,
                op,
                ty,
                lhs,
                rhs,
            } => write!(f, "%{dest} = {op} {ty} {lhs}, {rhs}"),
            Inst::FNeg { dest, value } => write!(f, "%{dest} = fneg {value}"),
            Inst::ICmp {
                dest,
                pred,
                ty,
                lhs,
                rhs,
            } => write!(f, "%{dest} = icmp {pred} {ty} {lhs}, {rhs}"),
            Inst::FCmp {
                dest,
                pred,
                ty,
                lhs,
                rhs,
            } => write!(f, "%{dest} = fcmp {pred} {ty} {lhs}, {rhs}"),
            Inst::Select {
                dest,
                cond,
                then,
                other,
            } => write!(f, "%{dest} = select i1 {cond}, {then}, {other}"),
            Inst::Cast {
                dest,
                op,
                value,
                to,
            } => write!(f, "%{dest} = {op} {value} to {to}"),
            Inst::Call { dest, call } => match dest {
                Some(dest) => write!(f, "%{dest} = {call}"),
                None => write!(f, "{call}"),
            },
            Inst::ExtractValue { dest, agg, index } => {
                write!(f, "%{dest} = extractvalue {agg}, {index}")
            }
            Inst::InsertValue {
                dest,
                agg,
                elem,
                index,
            } => write!(f, "%{dest} = insertvalue {agg}, {elem}, {index}"),
            Inst::LandingPad { dest } => {
                write!(f, "%{dest} = landingpad {} cleanup", landing_pad_ty())
            }
        }
    }
}

impl Display for Terminator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Ret(None) => f.write_str("ret void"),
            Terminator::Ret(Some(v)) => write!(f, "ret {v}"),
            Terminator::Invoke {
                dest,
                call,
                normal,
                unwind,
            } => {
                if let Some(dest) = dest {
                    write!(f, "%{dest} = ")?;
                }
                // A call, but `invoke`, and never `tail`.
                let call = call.to_string();
                let call = call.strip_prefix("tail ").unwrap_or(&call);
                let rest = call.strip_prefix("call").expect("a call");
                write!(f, "invoke{rest} to {normal} unwind {unwind}")
            }
            Terminator::Resume(v) => write!(f, "resume {} {v}", landing_pad_ty()),
            Terminator::Br(to) => write!(f, "br {to}"),
            Terminator::CondBr { cond, then, other } => {
                write!(f, "br i1 {cond}, {then}, {other}")
            }
            Terminator::Switch {
                value,
                default,
                cases,
            } => {
                write!(f, "switch {value}, {default} [")?;
                for (n, to) in cases {
                    write!(f, " {} {n}, {to}", value.0)?;
                }
                f.write_str(" ]")
            }
            Terminator::Unreachable => f.write_str("unreachable"),
        }
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let dbg = |loc: Option<u32>| loc.map_or(String::new(), |n| format!(", !dbg !{n}"));
        writeln!(f, "{}:", self.label.0)?;
        for (i, inst) in self.insts.iter().enumerate() {
            writeln!(f, "  {inst}{}", dbg(self.locs.get(i).copied().flatten()))?;
        }
        writeln!(f, "  {}{}", self.term, dbg(self.term_loc))
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "define {}{} @{}(", self.cc, self.ret, self.name)?;
        for (i, (t, name)) in self.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{t} %{name}")?;
        }
        f.write_str(")")?;
        for a in &self.attrs {
            write!(f, " {a}")?;
        }
        if let Some(p) = &self.personality {
            write!(f, " personality ptr {p}")?;
        }
        if let Some(n) = self.dbg {
            write!(f, " !dbg !{n}")?;
        }
        writeln!(f, " {{")?;
        for b in &self.blocks {
            write!(f, "{b}")?;
        }
        writeln!(f, "}}")
    }
}

impl Display for Declare {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "declare {} @{}({}",
            self.ret,
            self.name,
            List(&self.params)
        )?;
        if self.variadic {
            f.write_str(if self.params.is_empty() {
                "..."
            } else {
                ", ..."
            })?;
        }
        f.write_str(")")?;
        for a in &self.attrs {
            write!(f, " {a}")?;
        }
        writeln!(f)
    }
}

impl Display for Global {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "@{} = ", self.name)?;
        if self.private {
            f.write_str("private unnamed_addr ")?;
        }
        let kind = if self.constant { "constant" } else { "global" };
        write!(f, "{kind} {} {}", self.ty, self.init)?;
        if let Some(a) = self.align {
            write!(f, ", align {a}")?;
        }
        writeln!(f)
    }
}

impl Display for Item {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Item::Comment(text) => writeln!(f, "; {text}"),
            Item::Declare(d) => write!(f, "{d}"),
            Item::Global(g) => write!(f, "{g}"),
            Item::Function(func) => write!(f, "\n{func}"),
            Item::Text(text) => f.write_str(text),
        }
    }
}

impl Display for Module {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for item in &self.items {
            write!(f, "{item}")?;
        }
        if !self.metadata.is_empty() {
            writeln!(f)?;
            for line in &self.metadata {
                writeln!(f, "{line}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_a_function() {
        let mut b = FnBuilder::new(
            CallConv::Tail,
            Type::I32,
            "f",
            vec![(Type::Ptr, "p".into())],
        );
        let p = b.param(0);
        let q = b.offset(&p, 4);
        let x = b.load(Type::I32, q);
        let zero = b.icmp(IntPred::Eq, Type::I32, &x, 0);
        let (yes, no) = (b.new_block(), b.new_block());
        b.cond_br(zero, &yes, &no);
        b.start(&yes);
        b.unreachable();
        b.start(&no);
        b.ret(Some(Type::I32.val(x)));
        let text = b.finish().to_string();
        assert_eq!(
            text,
            "define tailcc i32 @f(ptr %p) {\n\
             entry:\n  \
             %t1 = getelementptr i8, ptr %p, i64 4\n  \
             %t2 = load i32, ptr %t1\n  \
             %t3 = icmp eq i32 %t2, 0\n  \
             br i1 %t3, label %b1, label %b2\n\
             b1:\n  unreachable\n\
             b2:\n  ret i32 %t2\n\
             }\n"
        );
    }

    #[test]
    fn prints_constants() {
        let g = Global {
            align: Some(8),
            ..Global::constant(
                "k",
                Type::Packed(vec![Type::I32, Type::Array(4, Box::new(Type::I8))]),
                Value::Packed(vec![
                    Type::I32.val(-1),
                    Typed(Type::Array(4, Box::new(Type::I8)), Value::Zero),
                ]),
            )
        };
        assert_eq!(
            g.to_string(),
            "@k = private unnamed_addr constant <{ i32, [4 x i8] }> \
             <{ i32 -1, [4 x i8] zeroinitializer }>, align 8\n"
        );
        assert_eq!(
            Value::Bytes(b"%llx \0".to_vec()).to_string(),
            "c\"%llx \\00\""
        );
    }
}
