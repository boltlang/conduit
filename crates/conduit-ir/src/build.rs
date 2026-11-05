//! A builder for functions, for frontends lowering their own IR to Conduit
//! and for passes that generate code.
//!
//! The builder lets code be emitted in the order a frontend walks its AST,
//! not in the order of the IR's tree. It keeps every block in a flat list
//! while building, with a cursor on the one that `let`s are added to. A
//! `join` or `case` ends the current block and gives back the new blocks it
//! opens, which are filled in later in any order. [`FuncBuilder::finish`]
//! puts the tree together.
//!
//! ```
//! # use conduit_ir::*;
//! # use conduit_ir::build::FuncBuilder;
//! // let x = if c { a } else { b }; x + 1
//! let m = Module::default();
//! let mut f = FuncBuilder::new(&m, vec![Ty::I32; 3], vec![Ty::I32]);
//! let [c, a, b] = f.params();
//! let k = f.join(&[Ty::I32]);
//! let case = f.case(c);
//! f.arm(case, 1);
//! f.jump(&k, [a]);
//! f.default(case);
//! f.jump(&k, [b]);
//! f.switch_to(k.block());
//! let y = f.add(k.param(0), 1i32);
//! f.ret([y]);
//! let func = f.finish().unwrap();
//! ```
//!
//! - **Result types are inferred,** by the same rules the verifier checks
//!   them with. Ops whose result the IR can't infer take it as an argument:
//!   the mutability of [`new`](FuncBuilder::new_object), of arrays and of
//!   closures.
//! - **Mistakes are collected,** not returned from every call: a type
//!   error, a `get` of a constructor an arm doesn't match, adding to a
//!   block that already ended. [`finish`](FuncBuilder::finish) returns
//!   them, with the source that was current when each happened.
//! - **Sources:** [`set_source`](FuncBuilder::set_source) sets the source
//!   of every `let` and `jump` that follows.
//!
//! A function that calls itself, or functions that call each other, must be
//! declared in the [`Program`] first, for example with a placeholder
//! [`Func::new`], so that the builder knows their signatures.

use std::fmt;

use crate::verify::fits;
use crate::verify::typing::op_results;
use crate::*;

/// A mistake found while building.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Error {
    /// The source that was current when it happened.
    pub source: Option<SourceId>,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(source) = self.source {
            write!(f, "{source}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// A block being built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockRef(u32);

/// A `case` being built, to add arms to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaseRef(u32);

/// A join point: its label, its parameters, and the block of its body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JoinPoint {
    label: Label,
    params: Vec<Var>,
    block: BlockRef,
}

impl JoinPoint {
    pub fn label(&self) -> Label {
        self.label
    }

    pub fn params(&self) -> &[Var] {
        &self.params
    }

    /// Parameter `i`. Panics if there's no such parameter.
    pub fn param(&self, i: usize) -> Var {
        self.params[i]
    }

    /// The block of the join point's body, which runs when it's jumped to.
    pub fn block(&self) -> BlockRef {
        self.block
    }
}

/// Values that can be the arguments of an op or a jump: an array or a
/// `Vec` of anything that converts to an [`Atom`], or `()` for none.
pub trait Atoms {
    fn atoms(self) -> Vec<Atom>;
}

impl Atoms for () {
    fn atoms(self) -> Vec<Atom> {
        Vec::new()
    }
}

impl<A: Into<Atom>, const N: usize> Atoms for [A; N] {
    fn atoms(self) -> Vec<Atom> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<A: Into<Atom>> Atoms for Vec<A> {
    fn atoms(self) -> Vec<Atom> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<A: Into<Atom> + Clone> Atoms for &[A] {
    fn atoms(self) -> Vec<Atom> {
        self.iter().cloned().map(Into::into).collect()
    }
}

impl From<Var> for Atom {
    fn from(v: Var) -> Atom {
        Atom::Var(v)
    }
}

impl From<Lit> for Atom {
    fn from(l: Lit) -> Atom {
        Atom::Lit(l)
    }
}

impl From<i8> for Atom {
    fn from(n: i8) -> Atom {
        Atom::Lit(Lit::I8(n))
    }
}

impl From<i16> for Atom {
    fn from(n: i16) -> Atom {
        Atom::Lit(Lit::I16(n))
    }
}

impl From<i32> for Atom {
    fn from(n: i32) -> Atom {
        Atom::Lit(Lit::I32(n))
    }
}

impl From<i64> for Atom {
    fn from(n: i64) -> Atom {
        Atom::Lit(Lit::I64(n))
    }
}

impl From<f32> for Atom {
    fn from(x: f32) -> Atom {
        Atom::Lit(Lit::f32(x))
    }
}

impl From<f64> for Atom {
    fn from(x: f64) -> Atom {
        Atom::Lit(Lit::f64(x))
    }
}

impl From<CtorId> for Tag {
    fn from(c: CtorId) -> Tag {
        Tag::Ctor(c)
    }
}

impl From<i64> for Tag {
    fn from(n: i64) -> Tag {
        Tag::Int(n)
    }
}

impl From<i32> for Tag {
    fn from(n: i32) -> Tag {
        Tag::Int(n.into())
    }
}

struct Open {
    lets: Vec<Let>,
    tail: Option<Pending>,
    /// The enclosing block, if any.
    parent: Option<BlockRef>,
    /// The constructor that a variable is known to have here, because
    /// this block is a `case` arm.
    known: Option<(Var, CtorId)>,
    /// What this block is, for messages.
    what: String,
}

/// A tail whose blocks may still be open.
enum Pending {
    Join {
        rec: bool,
        defs: Vec<(Label, Vec<Var>, BlockRef)>,
        body: BlockRef,
    },
    Jump {
        target: Label,
        args: Vec<Atom>,
        source: Option<SourceId>,
    },
    Case(CaseRef),
    Unreachable,
}

struct OpenCase {
    scrut: Atom,
    scrut_ty: Ty,
    arms: Vec<(Tag, BlockRef)>,
    default: Option<BlockRef>,
    /// The block this `case` ends.
    block: BlockRef,
}

/// Builds one function. See the [module docs](self).
pub struct FuncBuilder<'p, P: ?Sized> {
    p: &'p P,
    func: Func,
    blocks: Vec<Open>,
    cases: Vec<OpenCase>,
    /// The parameter types of each label made so far, by its number.
    label_params: Vec<Option<Vec<Ty>>>,
    current: BlockRef,
    source: Option<SourceId>,
    errors: Vec<Error>,
}

/// Emits a binary or unary scalar op.
macro_rules! prims {
    ($($(#[$doc:meta])* $name:ident($($arg:ident),*) => $prim:expr;)*) => {$(
        $(#[$doc])*
        pub fn $name(&mut self, $($arg: impl Into<Atom>),*) -> Var {
            self.prim($prim, [$($arg.into()),*])
        }
    )*};
}

impl<'p, P: Program + ?Sized> FuncBuilder<'p, P> {
    /// A builder for a function with these parameters and results, whose
    /// calls and data types are looked up in `p`.
    pub fn new(p: &'p P, params: Vec<Ty>, results: Vec<Ty>) -> Self {
        let label_params = vec![Some(results.clone())];
        FuncBuilder {
            p,
            func: Func::new(params, results),
            blocks: vec![Open {
                lets: Vec::new(),
                tail: None,
                parent: None,
                known: None,
                what: "the function's body".into(),
            }],
            cases: Vec::new(),
            label_params,
            current: BlockRef(0),
            source: None,
            errors: Vec::new(),
        }
    }

    /// Gives the function mutability parameters, which its types can use
    /// as [`Mutability::Param`]. Set them before emitting code that uses
    /// them.
    pub fn set_mut_params(&mut self, mp: MutParams) {
        self.func.mut_params = mp;
    }

    /// Gives the function `n` effect parameters, `E1` to `En`, which its
    /// rows can then use.
    pub fn set_effect_params(&mut self, n: u32) {
        self.func.effect_params = n;
    }

    /// The function's parameters. Panics if it doesn't have `N` of them.
    pub fn params<const N: usize>(&self) -> [Var; N] {
        self.func.params.as_slice().try_into().unwrap_or_else(|_| {
            panic!(
                "the function has {} parameters, not {N}",
                self.func.params.len()
            )
        })
    }

    /// Says what the function may perform: no effects, unless this is
    /// called.
    pub fn set_row(&mut self, row: Row) {
        self.func.row = row;
    }

    /// Parameter `i`. Panics if there's no such parameter.
    pub fn param(&self, i: usize) -> Var {
        self.func.params[i]
    }

    /// The type of a variable made by this builder.
    pub fn var_ty(&self, v: Var) -> Option<&Ty> {
        self.func.vars.get(v.0 as usize)
    }

    /// The block `let`s are added to.
    pub fn current(&self) -> BlockRef {
        self.current
    }

    /// Moves the cursor to `b`, so that what follows is added to it.
    pub fn switch_to(&mut self, b: BlockRef) {
        self.current = b;
    }

    /// The source of every `let` and `jump` from now on.
    pub fn set_source(&mut self, source: Option<SourceId>) {
        self.source = source;
    }

    /// Whether `b` has ended, with a jump, a `case`, a `join` or
    /// `unreachable`.
    pub fn is_ended(&self, b: BlockRef) -> bool {
        self.blocks[b.0 as usize].tail.is_some()
    }

    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(Error {
            source: self.source,
            message: message.into(),
        });
    }

    fn block(&mut self, parent: BlockRef, known: Option<(Var, CtorId)>, what: String) -> BlockRef {
        self.blocks.push(Open {
            lets: Vec::new(),
            tail: None,
            parent: Some(parent),
            known,
            what,
        });
        BlockRef(self.blocks.len() as u32 - 1)
    }

    /// Whether the current block can still be added to, reporting an
    /// error if not.
    fn open(&mut self, what: &str) -> bool {
        let b = &self.blocks[self.current.0 as usize];
        if b.tail.is_some() {
            let message = format!(
                "{what} added to {}, which has already ended: switch to another block first",
                b.what
            );
            self.error(message);
            return false;
        }
        true
    }

    fn end(&mut self, tail: Pending, what: &str) {
        if self.open(what) {
            self.blocks[self.current.0 as usize].tail = Some(tail);
        }
    }

    fn atom_ty(&self, a: &Atom) -> Option<Ty> {
        match a {
            Atom::Var(v) => self.var_ty(*v).cloned(),
            Atom::Lit(l) => Some(l.ty()),
        }
    }

    fn atom_tys(&mut self, atoms: &[Atom]) -> Option<Vec<Ty>> {
        let tys: Option<Vec<Ty>> = atoms.iter().map(|a| self.atom_ty(a)).collect();
        if tys.is_none() {
            self.error("an argument is a variable this builder didn't make");
        }
        tys
    }

    /// What the enclosing `case` arms say about variables.
    fn known(&self) -> Vec<(Var, CtorId)> {
        let mut known = Vec::new();
        let mut b = Some(self.current);
        while let Some(at) = b {
            let block = &self.blocks[at.0 as usize];
            known.extend(block.known);
            b = block.parent;
        }
        known
    }

    /// Adds `let binds = op(args)`, where `out` is the types to bind, or
    /// `None` to infer them. Gives the variables it binds.
    fn emit(&mut self, op: Op, args: Vec<Atom>, out: Option<Vec<Ty>>) -> Vec<Var> {
        let fallback = |b: &mut Self, out: Option<Vec<Ty>>| -> Vec<Var> {
            out.unwrap_or_default()
                .into_iter()
                .map(|ty| b.func.new_var(ty))
                .collect()
        };
        let Some(tys) = self.atom_tys(&args) else {
            return fallback(self, out);
        };
        let given = out.clone().unwrap_or_default();
        let results = match op_results(
            self.p,
            &self.func.mut_params,
            self.func.effect_params,
            &op,
            &args,
            &tys,
            &given,
            &self.known(),
        ) {
            Ok(results) => results,
            Err(message) if out.is_none() && needs_result_type(&op) => {
                self.error(format!("{message}: this op needs its result type given"));
                return Vec::new();
            }
            Err(message) => {
                self.error(message);
                return fallback(self, out);
            }
        };
        let binds_tys = match out {
            None => results,
            Some(out) => {
                let fits = results.len() == out.len()
                    && results.iter().zip(&out).all(|(r, o)| r.is_subtype(o));
                if !fits {
                    let list =
                        |tys: &[Ty]| tys.iter().map(Ty::to_string).collect::<Vec<_>>().join(", ");
                    self.error(format!(
                        "the op gives ({}), which doesn't fit ({})",
                        list(&results),
                        list(&out)
                    ));
                }
                out
            }
        };
        let binds: Vec<Var> = binds_tys
            .into_iter()
            .map(|ty| self.func.new_var(ty))
            .collect();
        if self.open("a let") {
            let source = self.source;
            self.blocks[self.current.0 as usize].lets.push(Let {
                binds: binds.clone(),
                op,
                args,
                source,
            });
        }
        binds
    }

    /// Like `emit`, for an op that gives one value.
    fn emit1(&mut self, op: Op, args: Vec<Atom>, out: Option<Ty>) -> Var {
        let before = self.errors.len();
        let binds = self.emit(op, args, out.map(|ty| vec![ty]));
        match binds[..] {
            [one] => one,
            _ => {
                // Unless the op itself was already reported.
                if self.errors.len() == before {
                    self.error(format!("the op gives {} values, not one", binds.len()));
                }
                self.func.new_var(Ty::I32)
            }
        }
    }

    /// `let xs = op(args)`, with the types of `xs` inferred.
    pub fn op(&mut self, op: Op, args: impl Atoms) -> Vec<Var> {
        self.emit(op, args.atoms(), None)
    }

    /// `let xs = op(args)`, with `xs` of types `results`. The op's results
    /// must fit them.
    pub fn op_as(&mut self, op: Op, args: impl Atoms, results: Vec<Ty>) -> Vec<Var> {
        self.emit(op, args.atoms(), Some(results))
    }

    pub fn prim(&mut self, p: Prim, args: impl Atoms) -> Var {
        self.emit1(Op::Prim(p), args.atoms(), None)
    }

    prims! {
        add(a, b) => Prim::Add;
        sub(a, b) => Prim::Sub;
        mul(a, b) => Prim::Mul;
        div_s(a, b) => Prim::Div(Sign::Signed);
        div_u(a, b) => Prim::Div(Sign::Unsigned);
        rem_s(a, b) => Prim::Rem(Sign::Signed);
        rem_u(a, b) => Prim::Rem(Sign::Unsigned);
        neg(a) => Prim::Neg;
        and(a, b) => Prim::And;
        or(a, b) => Prim::Or;
        xor(a, b) => Prim::Xor;
        shl(a, b) => Prim::Shl;
        shr_s(a, b) => Prim::Shr(Sign::Signed);
        shr_u(a, b) => Prim::Shr(Sign::Unsigned);
        eq(a, b) => Prim::Eq;
        ne(a, b) => Prim::Ne;
        lt_s(a, b) => Prim::Lt(Sign::Signed);
        lt_u(a, b) => Prim::Lt(Sign::Unsigned);
        le_s(a, b) => Prim::Le(Sign::Signed);
        le_u(a, b) => Prim::Le(Sign::Unsigned);
        gt_s(a, b) => Prim::Gt(Sign::Signed);
        gt_u(a, b) => Prim::Gt(Sign::Unsigned);
        ge_s(a, b) => Prim::Ge(Sign::Signed);
        ge_u(a, b) => Prim::Ge(Sign::Unsigned);
        abs(a) => Prim::Abs;
        sqrt(a) => Prim::Sqrt;
        min(a, b) => Prim::Min;
        max(a, b) => Prim::Max;
    }

    /// `a` converted to the scalar type `to`, by value.
    pub fn convert(&mut self, to: Ty, sign: Sign, a: impl Into<Atom>) -> Var {
        self.prim(Prim::Convert(to, sign), [a])
    }

    /// `a` as the scalar type `to` of the same size, by bits.
    pub fn reinterpret(&mut self, to: Ty, a: impl Into<Atom>) -> Var {
        self.prim(Prim::Reinterpret(to), [a])
    }

    pub fn call(&mut self, f: FuncId, args: impl Atoms) -> Vec<Var> {
        self.emit(Op::Call(f, Vec::new(), Vec::new()), args.atoms(), None)
    }

    /// A call of a function with one result.
    pub fn call1(&mut self, f: FuncId, args: impl Atoms) -> Var {
        self.emit1(Op::Call(f, Vec::new(), Vec::new()), args.atoms(), None)
    }

    /// A call of a function with mutability parameters, giving `margs` for
    /// them.
    pub fn call_at(&mut self, f: FuncId, margs: Vec<Mutability>, args: impl Atoms) -> Vec<Var> {
        self.emit(Op::Call(f, margs, Vec::new()), args.atoms(), None)
    }

    /// [`call_at`](Self::call_at) of a function with one result.
    pub fn call1_at(&mut self, f: FuncId, margs: Vec<Mutability>, args: impl Atoms) -> Var {
        self.emit1(Op::Call(f, margs, Vec::new()), args.atoms(), None)
    }

    /// A call of a function with mutability and effect parameters, giving
    /// `margs` and `rows` for them.
    pub fn call_with(
        &mut self,
        f: FuncId,
        margs: Vec<Mutability>,
        rows: Vec<Row>,
        args: impl Atoms,
    ) -> Vec<Var> {
        self.emit(Op::Call(f, margs, rows), args.atoms(), None)
    }

    pub fn call_extern(&mut self, e: ExternId, args: impl Atoms) -> Vec<Var> {
        self.emit(Op::CallExtern(e), args.atoms(), None)
    }

    /// A call of a host function with one result.
    pub fn call_extern1(&mut self, e: ExternId, args: impl Atoms) -> Var {
        self.emit1(Op::CallExtern(e), args.atoms(), None)
    }

    /// A closure of `f` with mutability `m`, capturing `captures` as its
    /// first parameters.
    pub fn closure(&mut self, f: FuncId, m: Mutability, captures: impl Atoms) -> Var {
        self.closure_with(f, Vec::new(), Vec::new(), m, captures)
    }

    /// A closure of a function with mutability parameters, giving `margs`
    /// for them.
    pub fn closure_at(
        &mut self,
        f: FuncId,
        margs: Vec<Mutability>,
        m: Mutability,
        captures: impl Atoms,
    ) -> Var {
        self.closure_with(f, margs, Vec::new(), m, captures)
    }

    /// A closure of a function with mutability and effect parameters,
    /// giving `margs` and `rows` for them.
    pub fn closure_with(
        &mut self,
        f: FuncId,
        margs: Vec<Mutability>,
        rows: Vec<Row>,
        m: Mutability,
        captures: impl Atoms,
    ) -> Var {
        let captures = captures.atoms();
        let Some(sig) = self.p.func_sig(f) else {
            self.error(format!("unknown function {f}"));
            return self.func.new_var(Ty::I32);
        };
        let sig = verify::subst_sig_rows(&verify::subst_sig(&sig, &margs), &rows);
        let n = captures.len().min(sig.params.len());
        let rest = Sig {
            row: sig.row,
            params: sig.params[n..].to_vec(),
            results: sig.results,
        };
        self.emit1(
            Op::Closure(f, margs, rows),
            captures,
            Some(Ty::Fn(Box::new(rest), m)),
        )
    }

    pub fn apply(&mut self, closure: impl Into<Atom>, args: impl Atoms) -> Vec<Var> {
        let mut all = vec![closure.into()];
        all.extend(args.atoms());
        self.emit(Op::Apply, all, None)
    }

    /// An `apply` of a closure with one result.
    pub fn apply1(&mut self, closure: impl Into<Atom>, args: impl Atoms) -> Var {
        let mut all = vec![closure.into()];
        all.extend(args.atoms());
        self.emit1(Op::Apply, all, None)
    }

    /// `new c(fields)`, as an object of mutability `m`.
    pub fn new_object(&mut self, c: CtorId, m: Mutability, fields: impl Atoms) -> Var {
        self.new_object_with(c, m, Vec::new(), fields)
    }

    /// `new c(fields)`, as an object of mutability `m` whose declaration
    /// has mutability parameters, with `args` for them.
    pub fn new_object_with(
        &mut self,
        c: CtorId,
        m: Mutability,
        args: Vec<Mutability>,
        fields: impl Atoms,
    ) -> Var {
        self.new_object_full(c, m, args, Vec::new(), fields)
    }

    /// `new c(fields)`, as an object of mutability `m` whose declaration
    /// has mutability and effect parameters, with `args` and `rows` for
    /// them.
    pub fn new_object_full(
        &mut self,
        c: CtorId,
        m: Mutability,
        args: Vec<Mutability>,
        rows: Vec<Row>,
        fields: impl Atoms,
    ) -> Var {
        let ty = Ty::Data(c.data, m, args, rows);
        self.emit1(Op::New(c), fields.atoms(), Some(ty))
    }

    /// The value of constant `c`, of its declared type.
    pub fn constant(&mut self, c: ConstId) -> Var {
        self.emit1(Op::Const(c), Vec::new(), None)
    }

    /// Field `i` of `obj`, which must be known to be built by `c`.
    pub fn get(&mut self, c: CtorId, i: u32, obj: impl Into<Atom>) -> Var {
        self.emit1(Op::Get(c, i), vec![obj.into()], None)
    }

    pub fn set(&mut self, c: CtorId, i: u32, obj: impl Into<Atom>, value: impl Into<Atom>) {
        self.emit(Op::Set(c, i), vec![obj.into(), value.into()], None);
    }

    /// Every field of `obj`, read now. For a `mut` object, a field read
    /// later may differ.
    pub fn fields(&mut self, c: CtorId, obj: impl Into<Atom>) -> Vec<Var> {
        let obj = obj.into();
        let n = self
            .p
            .data(c.data)
            .and_then(|d| d.ctors.get(c.index as usize))
            .map_or(0, |ctor| ctor.fields.len());
        (0..n as u32).map(|i| self.get(c, i, obj.clone())).collect()
    }

    /// An array of `elems`, of element type `elem` and mutability `m`.
    pub fn array_new(&mut self, elem: Ty, m: Mutability, elems: impl Atoms) -> Var {
        let ty = Ty::Array(Box::new(elem), m);
        self.emit1(Op::ArrayNew, elems.atoms(), Some(ty))
    }

    /// An array of `len` times `value`, of element type `elem` and
    /// mutability `m`.
    pub fn array_fill(
        &mut self,
        elem: Ty,
        m: Mutability,
        len: impl Into<Atom>,
        value: impl Into<Atom>,
    ) -> Var {
        let ty = Ty::Array(Box::new(elem), m);
        self.emit1(Op::ArrayFill, vec![len.into(), value.into()], Some(ty))
    }

    /// A `mut` array of `len` elements of type `elem` that aren't set yet.
    pub fn array_alloc(&mut self, elem: Ty, len: impl Into<Atom>) -> Var {
        let ty = Ty::Array(Box::new(elem), Mutability::Mut);
        self.emit1(Op::ArrayAlloc, vec![len.into()], Some(ty))
    }

    /// Copies `count` elements of `src` from `src_start` into `dst` from
    /// `dst_start`.
    pub fn array_copy(
        &mut self,
        dst: impl Into<Atom>,
        dst_start: impl Into<Atom>,
        src: impl Into<Atom>,
        src_start: impl Into<Atom>,
        count: impl Into<Atom>,
    ) {
        let args = vec![
            dst.into(),
            dst_start.into(),
            src.into(),
            src_start.into(),
            count.into(),
        ];
        self.emit(Op::ArrayCopy, args, None);
    }

    pub fn array_get(&mut self, array: impl Into<Atom>, index: impl Into<Atom>) -> Var {
        self.emit1(Op::ArrayGet, vec![array.into(), index.into()], None)
    }

    pub fn array_set(
        &mut self,
        array: impl Into<Atom>,
        index: impl Into<Atom>,
        value: impl Into<Atom>,
    ) {
        self.emit(
            Op::ArraySet,
            vec![array.into(), index.into(), value.into()],
            None,
        );
    }

    pub fn array_len(&mut self, array: impl Into<Atom>) -> Var {
        self.emit1(Op::ArrayLen, vec![array.into()], None)
    }

    /// A new array of mutability `m` with the elements of `array`, which it
    /// shares.
    pub fn array_clone(&mut self, m: Mutability, array: impl Into<Atom>) -> Var {
        let array = array.into();
        let elem = self
            .atom_ty(&array)
            .as_ref()
            .and_then(crate::verify::elem_ty);
        let out = elem.map(|elem| Ty::Array(Box::new(elem), m));
        self.emit1(Op::ArrayClone, vec![array], out)
    }

    /// A ref holding `value`, of `value`'s type.
    pub fn ref_new(&mut self, value: impl Into<Atom>) -> Var {
        let value = value.into();
        match self.atom_ty(&value) {
            Some(ty) => self.ref_new_as(ty, value),
            None => self.emit1(Op::RefNew, vec![value], None),
        }
    }

    /// A ref of `ty` holding `value`. A mutable local of the source
    /// language is one of these (rule 6 in `docs/ir.md`).
    pub fn ref_new_as(&mut self, ty: Ty, value: impl Into<Atom>) -> Var {
        self.emit1(Op::RefNew, vec![value.into()], Some(Ty::Ref(Box::new(ty))))
    }

    pub fn ref_get(&mut self, r: impl Into<Atom>) -> Var {
        self.emit1(Op::RefGet, vec![r.into()], None)
    }

    pub fn ref_set(&mut self, r: impl Into<Atom>, value: impl Into<Atom>) {
        self.emit(Op::RefSet, vec![r.into(), value.into()], None);
    }

    /// `value`, of type `ty`, as an `any`.
    pub fn boxed(&mut self, ty: Ty, value: impl Into<Atom>) -> Var {
        self.emit1(Op::Box(ty), vec![value.into()], None)
    }

    /// An `any` as a value of type `ty`.
    pub fn unbox(&mut self, ty: Ty, value: impl Into<Atom>) -> Var {
        self.emit1(Op::Unbox(ty), vec![value.into()], None)
    }

    /// The array `value` as `Imm`, made in place: see [`Op::ArrayFreeze`].
    pub fn array_freeze(&mut self, value: impl Into<Atom>) -> Var {
        self.emit1(Op::ArrayFreeze, vec![value.into()], None)
    }

    /// Runs a closure under handler `h`. `args` is the handler's
    /// environment, then the closure.
    pub fn handle(&mut self, h: HandlerId, args: impl Atoms) -> Vec<Var> {
        self.handle_with(h, Vec::new(), args)
    }

    /// Runs a closure under handler `h`, which has effect parameters,
    /// giving `rows` for them.
    pub fn handle_with(&mut self, h: HandlerId, rows: Vec<Row>, args: impl Atoms) -> Vec<Var> {
        self.emit(Op::Handle(h, rows), args.atoms(), None)
    }

    pub fn perform(&mut self, e: EffectId, op: u32, args: impl Atoms) -> Vec<Var> {
        self.emit(Op::Perform(e, op), args.atoms(), None)
    }

    pub fn resume(&mut self, k: impl Into<Atom>, args: impl Atoms) -> Vec<Var> {
        let mut all = vec![k.into()];
        all.extend(args.atoms());
        self.emit(Op::Resume, all, None)
    }

    /// Continues `k` by performing operation `op` of `e` where it was
    /// captured.
    pub fn discontinue(
        &mut self,
        k: impl Into<Atom>,
        e: EffectId,
        op: u32,
        args: impl Atoms,
    ) -> Vec<Var> {
        let mut all = vec![k.into()];
        all.extend(args.atoms());
        self.emit(Op::Discontinue(e, op), all, None)
    }

    /// Ends the current block by returning `values`.
    pub fn ret(&mut self, values: impl Atoms) {
        self.jump_to(Label::RET, values.atoms());
    }

    /// Ends the current block with a jump to `j`.
    pub fn jump(&mut self, j: &JoinPoint, args: impl Atoms) {
        self.jump_to(j.label, args.atoms());
    }

    fn jump_to(&mut self, target: Label, args: Vec<Atom>) {
        let params = self.label_params.get(target.0 as usize).cloned().flatten();
        match (params, self.atom_tys(&args)) {
            (Some(params), Some(tys)) => {
                if let Err(message) = fits(&tys, &params) {
                    self.error(format!("jump to {target}: {message}"));
                }
            }
            (None, _) => self.error(format!("jump to {target}, which this builder didn't make")),
            (_, None) => {}
        }
        let source = self.source;
        self.end(
            Pending::Jump {
                target,
                args,
                source,
            },
            "a jump",
        );
    }

    /// Ends the current block with `unreachable`.
    pub fn unreachable(&mut self) {
        self.end(Pending::Unreachable, "unreachable");
    }

    /// Ends the current block with `join j(params) = .. in ..`, and moves
    /// the cursor to the join's body. `j`'s own body is
    /// [`JoinPoint::block`], to fill in later.
    pub fn join(&mut self, params: &[Ty]) -> JoinPoint {
        self.join_group(false, &[params]).remove(0)
    }

    /// Like [`join`](Self::join), for a recursive join point: it can jump
    /// to itself, as a loop does.
    pub fn join_rec(&mut self, params: &[Ty]) -> JoinPoint {
        self.join_group(true, &[params]).remove(0)
    }

    /// Ends the current block with a join of several join points, one per
    /// parameter list, and moves the cursor to the join's body. If `rec`,
    /// they can jump to each other.
    pub fn join_group(&mut self, rec: bool, params: &[&[Ty]]) -> Vec<JoinPoint> {
        let at = self.current;
        let mut points = Vec::new();
        for tys in params {
            let label = self.func.new_label();
            let vars = tys.iter().map(|ty| self.func.new_var(ty.clone())).collect();
            let block = self.block(at, None, format!("the body of {label}"));
            self.label_params.resize(label.0 as usize + 1, None);
            self.label_params[label.0 as usize] = Some(tys.to_vec());
            points.push(JoinPoint {
                label,
                params: vars,
                block,
            });
        }
        let what = match &points[..] {
            [one] => format!("the code after join {}", one.label),
            _ => "the code after a join".into(),
        };
        let body = self.block(at, None, what);
        let defs = points
            .iter()
            .map(|j| (j.label, j.params.clone(), j.block))
            .collect();
        self.end(Pending::Join { rec, defs, body }, "a join");
        self.current = body;
        points
    }

    /// Ends the current block with a `case` on `scrut`, which has no arms
    /// yet: add them with [`arm`](Self::arm), [`arms`](Self::arms) and
    /// [`default`](Self::default).
    pub fn case(&mut self, scrut: impl Into<Atom>) -> CaseRef {
        let scrut = scrut.into();
        let scrut_ty = self.atom_ty(&scrut).unwrap_or_else(|| {
            self.error("the case is on a variable this builder didn't make");
            Ty::I32
        });
        if !matches!(scrut_ty, Ty::I32 | Ty::I64 | Ty::Data(..)) {
            self.error(format!("can't branch on a value of type {scrut_ty}"));
        }
        let case = CaseRef(self.cases.len() as u32);
        self.cases.push(OpenCase {
            scrut,
            scrut_ty,
            arms: Vec::new(),
            default: None,
            block: self.current,
        });
        self.end(Pending::Case(case), "a case");
        case
    }

    /// Adds an arm for `tag` to `case`, and moves the cursor to it.
    pub fn arm(&mut self, case: CaseRef, tag: impl Into<Tag>) -> BlockRef {
        let tag = tag.into();
        self.check_tag(case, tag);
        let c = &self.cases[case.0 as usize];
        let known = match (&c.scrut, tag) {
            (Atom::Var(v), Tag::Ctor(ctor)) => Some((*v, ctor)),
            _ => None,
        };
        let what = format!("the arm {} of the case on {}", tag_text(tag), c.scrut);
        let b = self.block(c.block, known, what);
        self.cases[case.0 as usize].arms.push((tag, b));
        self.current = b;
        b
    }

    /// Adds one arm for several tags to `case`, and moves the cursor to it.
    /// The IR has one tag per arm, so this puts the arm's body in a join
    /// point around the `case`, and each tag's arm jumps to it.
    pub fn arms<T: Into<Tag>>(
        &mut self,
        case: CaseRef,
        tags: impl IntoIterator<Item = T>,
    ) -> BlockRef {
        let tags: Vec<Tag> = tags.into_iter().map(Into::into).collect();
        if let [one] = tags[..] {
            return self.arm(case, one);
        }
        for &tag in &tags {
            self.check_tag(case, tag);
        }
        let holder = self.cases[case.0 as usize].block;
        let scrut = self.cases[case.0 as usize].scrut.clone();
        let label = self.func.new_label();
        self.label_params.resize(label.0 as usize + 1, None);
        self.label_params[label.0 as usize] = Some(Vec::new());
        let names: Vec<String> = tags.iter().map(|&t| tag_text(t)).collect();
        let shared = self.block(
            holder,
            None,
            format!("the arm {} of the case on {scrut}", names.join(" | ")),
        );
        // `holder` ended with the case. Now it ends with a join around it.
        let inner = self.block(holder, None, format!("the case on {scrut}"));
        self.blocks[inner.0 as usize].tail = Some(Pending::Case(case));
        self.blocks[holder.0 as usize].tail = Some(Pending::Join {
            rec: false,
            defs: vec![(label, Vec::new(), shared)],
            body: inner,
        });
        self.cases[case.0 as usize].block = inner;
        for tag in tags {
            let b = self.block(
                inner,
                None,
                format!("the arm {} of the case on {scrut}", tag_text(tag)),
            );
            self.blocks[b.0 as usize].tail = Some(Pending::Jump {
                target: label,
                args: Vec::new(),
                source: self.source,
            });
            self.cases[case.0 as usize].arms.push((tag, b));
        }
        self.current = shared;
        shared
    }

    /// Adds the default arm to `case`, and moves the cursor to it.
    pub fn default(&mut self, case: CaseRef) -> BlockRef {
        let c = &self.cases[case.0 as usize];
        let (holder, what) = (
            c.block,
            format!("the default arm of the case on {}", c.scrut),
        );
        if c.default.is_some() {
            let message = format!("the case on {} already has a default arm", c.scrut);
            self.error(message);
        }
        let b = self.block(holder, None, what);
        self.cases[case.0 as usize].default = Some(b);
        self.current = b;
        b
    }

    fn check_tag(&mut self, case: CaseRef, tag: Tag) {
        let c = &self.cases[case.0 as usize];
        let (scrut, ty) = (&c.scrut, &c.scrut_ty);
        let problem = match (ty, tag) {
            _ if c.arms.iter().any(|(t, _)| *t == tag) => Some(format!(
                "the case on {scrut} already has an arm {}",
                tag_text(tag)
            )),
            (Ty::I32, Tag::Int(n)) if i32::try_from(n).is_err() => Some(format!(
                "the case on {scrut} is on an i32, so it can't have a tag {n}"
            )),
            (Ty::I32 | Ty::I64, Tag::Int(_)) => None,
            (Ty::Data(d, ..), Tag::Ctor(ctor)) => {
                let ctors = self.p.data(*d).map_or(0, |decl| decl.ctors.len());
                (ctor.data != *d || ctor.index as usize >= ctors)
                    .then(|| format!("{ctor} isn't a constructor of {d}"))
            }
            _ => Some(format!(
                "the case on {scrut} is on a {ty}, so it can't have a tag {}",
                tag_text(tag)
            )),
        };
        if let Some(problem) = problem {
            self.error(problem);
        }
    }

    /// The finished function, or every mistake found while building it.
    pub fn finish(mut self) -> Result<Func, Vec<Error>> {
        for i in 0..self.cases.len() {
            let c = &self.cases[i];
            if c.default.is_some() {
                continue;
            }
            let missing = match &c.scrut_ty {
                Ty::Data(d, ..) => {
                    let ctors = self.p.data(*d).map_or(0, |decl| decl.ctors.len()) as u32;
                    (0..ctors).any(|index| {
                        let tag = Tag::Ctor(CtorId { data: *d, index });
                        !c.arms.iter().any(|(t, _)| *t == tag)
                    })
                }
                _ => true,
            };
            if missing {
                let message = format!(
                    "the case on {} has no default arm and doesn't cover every value",
                    c.scrut
                );
                self.error(message);
            }
        }
        for i in 0..self.blocks.len() {
            if self.blocks[i].tail.is_none() {
                let message = format!("{} never ends: it needs a jump", self.blocks[i].what);
                self.error(message);
            }
        }
        if !self.errors.is_empty() {
            return Err(self.errors);
        }
        let mut func = self.func;
        func.body = assemble(&mut self.blocks, &self.cases, BlockRef(0));
        Ok(func)
    }

    /// Like [`finish`](Self::finish), then runs the verifier on the
    /// function, as function `id` of the program.
    pub fn finish_verified(self, id: FuncId) -> Result<Func, Vec<Error>> {
        let p = self.p;
        let func = self.finish()?;
        verify::verify_func(p, id, &func).map_err(|errors| {
            errors
                .into_iter()
                .map(|e| Error {
                    source: e.source,
                    message: e.message,
                })
                .collect::<Vec<_>>()
        })?;
        Ok(func)
    }
}

/// Whether an op's result type depends on what it binds, so it can't be
/// inferred from its arguments.
fn needs_result_type(op: &Op) -> bool {
    matches!(
        op,
        Op::New(_) | Op::ArrayNew | Op::ArrayFill | Op::Closure(..) | Op::RefNew
    )
}

fn tag_text(tag: Tag) -> String {
    match tag {
        Tag::Int(n) => n.to_string(),
        Tag::Ctor(c) => c.to_string(),
    }
}

/// Builds the block tree from `b` down. Every block has a tail, which
/// `finish` checks first.
fn assemble(blocks: &mut [Open], cases: &[OpenCase], b: BlockRef) -> Block {
    let open = &mut blocks[b.0 as usize];
    let lets = std::mem::take(&mut open.lets);
    let tail = match open
        .tail
        .take()
        .expect("finish checks that every block ends")
    {
        Pending::Join { rec, defs, body } => Tail::Join {
            rec,
            defs: defs
                .into_iter()
                .map(|(label, params, block)| JoinDef {
                    label,
                    params,
                    body: assemble(blocks, cases, block),
                })
                .collect(),
            body: Box::new(assemble(blocks, cases, body)),
        },
        Pending::Jump {
            target,
            args,
            source,
        } => Tail::Jump {
            target,
            args,
            source,
        },
        Pending::Case(c) => {
            let c = &cases[c.0 as usize];
            Tail::Case {
                scrut: c.scrut.clone(),
                arms: c
                    .arms
                    .iter()
                    .map(|&(tag, block)| Arm {
                        tag,
                        body: assemble(blocks, cases, block),
                    })
                    .collect(),
                default: c.default.map(|d| Box::new(assemble(blocks, cases, d))),
            }
        }
        Pending::Unreachable => Tail::Unreachable,
    };
    Block { lets, tail }
}

#[cfg(test)]
mod tests;
