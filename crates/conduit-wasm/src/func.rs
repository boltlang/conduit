//! How a function body becomes Wasm code. See "Lowering" in `docs/ir.md`.
//!
//! Every variable is a local, and every term ends in a `return`, `br`,
//! `return_call` or `unreachable`, so blocks never carry values and no
//! code falls through. Join points become blocks:
//!
//! - `join j(..) = D in B` is `block B end` then `D`: a jump in `B` sets
//!   `j`'s parameters and leaves the block.
//! - `join rec j(..) = D in B` is `block B end` then `loop D end`. The
//!   block and the loop are at the same depth, so a jump to `j` is the same
//!   `br` in both: in `B` it leaves the block and enters the loop, and in
//!   `D` it goes back to the start of the loop.
//! - `join rec j1(..) = D1, .., jn(..) = Dn in B` does the same, with the
//!   loop dispatching on a local saying which label was jumped to.
//!
//! A jump pushes all its arguments before setting any parameter, so a
//! jump like `loop(y, x)` that swaps two parameters works.

use std::borrow::Cow;

use conduit_ir::{Atom, CtorId, Func, JoinDef, Label, Lit, Op, Prim, Sign, Tag, Term, Ty, Var};
use wasm_encoder::{BlockType, HeapType, Ieee32, Ieee64, Instruction as I, RefType, ValType};

use crate::Cx;
use crate::types::nullable;

/// A lowered function: the locals after its parameters, and its code.
pub(crate) struct Body {
    pub locals: Vec<ValType>,
    pub code: Vec<I<'static>>,
}

/// Where a jump to a label goes.
#[derive(Clone)]
struct Target {
    /// The depth of the `block` or `loop` to branch to.
    level: u32,
    params: Vec<Var>,
    /// For a group of recursive join points: the local saying which one
    /// was jumped to, and this one's number.
    which: Option<(u32, i32)>,
}

struct FuncCx<'a, 'm> {
    cx: &'a mut Cx<'m>,
    f: &'m Func,
    /// The local of each variable.
    locals: Vec<u32>,
    /// The types of the locals after the parameters.
    extra: Vec<ValType>,
    results: Vec<ValType>,
    labels: Vec<Option<Target>>,
    /// How many blocks and loops are open.
    depth: u32,
    code: Vec<I<'static>>,
}

/// Case arms on a dense range of `i32` tags dispatch with `br_table` when
/// there are at least this many.
const MIN_TABLE_ARMS: usize = 4;

pub(crate) fn lower_func<'m>(cx: &mut Cx<'m>, f: &'m Func) -> Result<Body, String> {
    let mut locals = vec![u32::MAX; f.vars.len()];
    for (i, p) in f.params.iter().enumerate() {
        locals[p.0 as usize] = i as u32;
    }
    let mut extra = Vec::new();
    let next = f.params.len() as u32;
    for (v, ty) in f.vars.iter().enumerate() {
        if locals[v] == u32::MAX {
            locals[v] = next + extra.len() as u32;
            extra.push(cx.types.val(ty)?);
        }
    }
    let results = cx.types.vals(&f.results)?;
    let mut fcx = FuncCx {
        cx,
        f,
        locals,
        extra,
        results,
        labels: vec![None; f.labels.max(1) as usize],
        depth: 0,
        code: Vec::new(),
    };
    fcx.term(&f.body)?;
    fcx.code.push(I::End);
    Ok(Body {
        locals: fcx.extra,
        code: fcx.code,
    })
}

/// The wrapper that closures of `f` with `n` captured values call: it reads
/// the captured values out of the closure and tail-calls `f`.
pub(crate) fn lower_entry(
    cx: &mut Cx<'_>,
    f: conduit_ir::FuncId,
    n: usize,
) -> Result<Body, String> {
    let (captures, sig) = cx.types.captures(f, n)?;
    let env = sig.params.len() as u32 + 1;
    let mut code = vec![
        I::LocalGet(0),
        I::RefCastNullable(HeapType::Concrete(captures)),
        I::LocalSet(env),
    ];
    for i in 0..n as u32 {
        code.push(I::LocalGet(env));
        code.push(I::StructGet {
            struct_type_index: captures,
            field_index: 1 + i,
        });
    }
    for p in 0..sig.params.len() as u32 {
        code.push(I::LocalGet(1 + p));
    }
    code.push(I::ReturnCall(cx.func_index(f)?));
    code.push(I::End);
    Ok(Body {
        locals: vec![nullable(captures)],
        code,
    })
}

impl<'m> FuncCx<'_, 'm> {
    fn emit(&mut self, i: I<'static>) {
        self.code.push(i);
    }

    fn open(&mut self, i: I<'static>) {
        self.code.push(i);
        self.depth += 1;
    }

    fn close(&mut self) {
        self.code.push(I::End);
        self.depth -= 1;
    }

    fn local(&self, v: Var) -> u32 {
        self.locals[v.0 as usize]
    }

    fn var_ty(&self, v: Var) -> &'m Ty {
        let f: &'m Func = self.f;
        f.var_ty(v)
    }

    fn atom_ty(&self, a: &Atom) -> Ty {
        match a {
            Atom::Var(v) => self.var_ty(*v).clone(),
            Atom::Lit(l) => l.ty(),
        }
    }

    fn atom(&mut self, a: &Atom) {
        let i = match *a {
            Atom::Var(v) => I::LocalGet(self.local(v)),
            Atom::Lit(Lit::I32(n)) => I::I32Const(n),
            Atom::Lit(Lit::I64(n)) => I::I64Const(n),
            Atom::Lit(Lit::F32(bits)) => I::F32Const(Ieee32::new(bits)),
            Atom::Lit(Lit::F64(bits)) => I::F64Const(Ieee64::new(bits)),
        };
        self.emit(i);
    }

    fn atoms(&mut self, atoms: &[Atom]) {
        for a in atoms {
            self.atom(a);
        }
    }

    fn term(&mut self, mut t: &'m Term) -> Result<(), String> {
        // Iterate over chains of `let`s, so that a long function doesn't
        // recurse once per statement.
        loop {
            match t {
                Term::Let {
                    binds,
                    op,
                    args,
                    body,
                    ..
                } => {
                    if self.let_(binds, op, args, body)? {
                        return Ok(());
                    }
                    t = body;
                }
                Term::Join { rec, defs, body } => return self.join(*rec, defs, body),
                Term::Jump { target, args, .. } => return self.jump(*target, args),
                Term::Case {
                    scrut,
                    arms,
                    default,
                } => return self.case(scrut, arms, default.as_deref()),
                Term::Unreachable => {
                    self.emit(I::Unreachable);
                    return Ok(());
                }
            }
        }
    }

    fn jump(&mut self, target: Label, args: &[Atom]) -> Result<(), String> {
        self.atoms(args);
        if target == Label::RET {
            self.emit(I::Return);
            return Ok(());
        }
        let Some(t) = self.labels.get(target.0 as usize).cloned().flatten() else {
            return Err(format!("jump to {target}, which isn't in scope"));
        };
        for &p in t.params.iter().rev() {
            self.emit(I::LocalSet(self.local(p)));
        }
        if let Some((which, n)) = t.which {
            self.emit(I::I32Const(n));
            self.emit(I::LocalSet(which));
        }
        self.emit(I::Br(self.depth - 1 - t.level));
        Ok(())
    }

    fn set_label(&mut self, def: &JoinDef, level: u32, which: Option<(u32, i32)>) {
        self.labels[def.label.0 as usize] = Some(Target {
            level,
            params: def.params.clone(),
            which,
        });
    }

    fn join(&mut self, rec: bool, defs: &'m [JoinDef], body: &'m Term) -> Result<(), String> {
        let base = self.depth;
        if !rec {
            // The innermost block is `defs[0]`'s, so that its body comes
            // right after the join body.
            for (i, def) in defs.iter().enumerate() {
                self.set_label(def, base + (defs.len() - 1 - i) as u32, None);
            }
            for _ in defs {
                self.open(I::Block(BlockType::Empty));
            }
            self.term(body)?;
            for def in defs {
                self.close();
                self.term(&def.body)?;
            }
            return Ok(());
        }

        let which = match defs {
            [def] => {
                self.set_label(def, base, None);
                None
            }
            _ => {
                let which = self.new_local(ValType::I32);
                for (i, def) in defs.iter().enumerate() {
                    self.set_label(def, base, Some((which, i as i32)));
                }
                Some(which)
            }
        };
        self.open(I::Block(BlockType::Empty));
        self.term(body)?;
        self.close();
        self.open(I::Loop(BlockType::Empty));
        match which {
            None => self.term(&defs[0].body)?,
            Some(which) => {
                for _ in defs {
                    self.open(I::Block(BlockType::Empty));
                }
                self.emit(I::LocalGet(which));
                let targets: Vec<u32> = (0..defs.len() as u32).collect();
                self.emit(I::BrTable(Cow::Owned(targets), defs.len() as u32 - 1));
                for def in defs {
                    self.close();
                    self.term(&def.body)?;
                }
            }
        }
        self.close();
        // The loop's body never falls through, but Wasm doesn't know that.
        self.emit(I::Unreachable);
        Ok(())
    }

    fn new_local(&mut self, ty: ValType) -> u32 {
        self.extra.push(ty);
        (self.f.params.len() + self.extra.len() - 1) as u32
    }

    fn case(
        &mut self,
        scrut: &Atom,
        arms: &'m [conduit_ir::Arm],
        default: Option<&'m Term>,
    ) -> Result<(), String> {
        let ty = self.atom_ty(scrut);
        let data = match ty {
            Ty::Data(d, _) => Some(d),
            _ => None,
        };
        // A value of an ADT with one constructor can only be that one.
        let only = match data {
            Some(d) if !self.cx.types.tagged(d)? => Some(arms.first().map(|a| &a.body).or(default)),
            _ if arms.is_empty() => Some(default),
            _ => None,
        };
        if let Some(only) = only {
            return match only {
                Some(t) => self.term(t),
                None => {
                    self.emit(I::Unreachable);
                    Ok(())
                }
            };
        }

        // One block per arm, then one for the default: breaking out of the
        // `i`th block from the innermost runs arm `i`.
        let n = arms.len();
        let targets = n + default.is_some() as usize;
        for _ in 0..targets {
            self.open(I::Block(BlockType::Empty));
        }
        let fallback = if default.is_some() { n as u32 } else { 0 };
        match data {
            Some(d) => {
                let base = self.cx.types.data(d)?;
                let ctors = self.cx.m.data[&d].ctors.len() as u32;
                let table = (0..ctors)
                    .map(|index| {
                        let tag = Tag::Ctor(CtorId { data: d, index });
                        arms.iter()
                            .position(|a| a.tag == tag)
                            .map_or(fallback, |i| i as u32)
                    })
                    .collect();
                self.atom(scrut);
                self.emit(I::StructGet {
                    struct_type_index: base,
                    field_index: 0,
                });
                self.emit(I::BrTable(Cow::Owned(table), fallback));
            }
            None => self.int_dispatch(scrut, &ty, arms, default.is_some())?,
        }
        for arm in arms {
            self.close();
            self.term(&arm.body)?;
        }
        if let Some(default) = default {
            self.close();
            self.term(default)?;
        }
        Ok(())
    }

    fn int_dispatch(
        &mut self,
        scrut: &Atom,
        ty: &Ty,
        arms: &[conduit_ir::Arm],
        has_default: bool,
    ) -> Result<(), String> {
        let n = arms.len();
        let mut tags = Vec::with_capacity(n);
        for arm in arms {
            match arm.tag {
                Tag::Int(t) => tags.push(t),
                Tag::Ctor(c) => return Err(format!("case on {ty} has a constructor tag {c}")),
            }
        }
        let (min, max) = (*tags.iter().min().unwrap(), *tags.iter().max().unwrap());
        let dense = *ty == Ty::I32 && n >= MIN_TABLE_ARMS && max - min < 2 * n as i64;
        if dense && has_default {
            let table = (min..=max)
                .map(|v| tags.iter().position(|&t| t == v).unwrap_or(n) as u32)
                .collect();
            self.atom(scrut);
            if min != 0 {
                self.emit(I::I32Const(min as i32));
                self.emit(I::I32Sub);
            }
            self.emit(I::BrTable(Cow::Owned(table), n as u32));
            return Ok(());
        }
        for (i, &tag) in tags.iter().enumerate() {
            self.atom(scrut);
            match ty {
                Ty::I32 => {
                    self.emit(I::I32Const(tag as i32));
                    self.emit(I::I32Eq);
                }
                Ty::I64 => {
                    self.emit(I::I64Const(tag));
                    self.emit(I::I64Eq);
                }
                _ => return Err(format!("can't branch on a value of type {ty}")),
            }
            self.emit(I::BrIf(i as u32));
        }
        self.emit(if has_default {
            I::Br(n as u32)
        } else {
            I::Unreachable
        });
        Ok(())
    }

    /// Lowers a `let`. Returns whether it was a tail call, which also
    /// covers the `jump ret` in its body.
    fn let_(
        &mut self,
        binds: &[Var],
        op: &'m Op,
        args: &[Atom],
        body: &Term,
    ) -> Result<bool, String> {
        let tail = self.is_tail(binds, body)?;
        match op {
            Op::Prim(p) => self.prim(p, args)?,
            Op::Call(f) => {
                let idx = self.cx.func_index(*f)?;
                self.atoms(args);
                self.emit(if tail {
                    I::ReturnCall(idx)
                } else {
                    I::Call(idx)
                });
                return Ok(self.bind(binds, tail));
            }
            Op::CallExtern(e) => {
                let idx = self.cx.extern_index(*e)?;
                self.atoms(args);
                self.emit(if tail {
                    I::ReturnCall(idx)
                } else {
                    I::Call(idx)
                });
                return Ok(self.bind(binds, tail));
            }
            Op::Closure(f) => {
                let (entry, captures) = self.cx.entry(*f, args.len())?;
                self.emit(I::RefFunc(entry));
                self.atoms(args);
                self.emit(I::StructNew(captures));
            }
            Op::Apply => {
                let Some(c) = args.first() else {
                    return Err("apply needs a closure".into());
                };
                let Ty::Fn(sig, _) = self.atom_ty(c) else {
                    return Err("apply of a value that isn't a closure".into());
                };
                let sig = self.cx.types.sig(&sig)?;
                let base = self.cx.types.closure(&sig)?;
                let entry = self.cx.types.entry(&sig)?;
                self.atoms(args);
                self.atom(c);
                self.emit(I::StructGet {
                    struct_type_index: base,
                    field_index: 0,
                });
                self.emit(if tail {
                    I::ReturnCallRef(entry)
                } else {
                    I::CallRef(entry)
                });
                return Ok(self.bind(binds, tail));
            }
            Op::New(c) => {
                let (idx, first) = self.cx.types.ctor(*c)?;
                if first == 1 {
                    self.emit(I::I32Const(c.index as i32));
                }
                self.atoms(args);
                self.emit(I::StructNew(idx));
            }
            Op::Get(c, i) => {
                let (idx, first) = self.object(*c, &args[0])?;
                self.emit(I::StructGet {
                    struct_type_index: idx,
                    field_index: first + i,
                });
            }
            Op::Set(c, i) => {
                let (idx, first) = self.object(*c, &args[0])?;
                self.atom(&args[1]);
                self.emit(I::StructSet {
                    struct_type_index: idx,
                    field_index: first + i,
                });
            }
            Op::ArrayNew => {
                let array = self.array_of(self.var_ty(binds[0]))?;
                self.atoms(args);
                self.emit(I::ArrayNewFixed {
                    array_type_index: array,
                    array_size: args.len() as u32,
                });
            }
            Op::ArrayFill => {
                let array = self.array_of(self.var_ty(binds[0]))?;
                // Wasm takes the value first, then the length.
                self.atom(&args[1]);
                self.atom(&args[0]);
                self.emit(I::ArrayNew(array));
            }
            Op::ArrayGet | Op::ArraySet => {
                let array = self.array_of(&self.atom_ty(&args[0]))?;
                self.atoms(args);
                self.emit(match op {
                    Op::ArrayGet => I::ArrayGet(array),
                    _ => I::ArraySet(array),
                });
            }
            Op::ArrayLen => {
                self.atoms(args);
                self.emit(I::ArrayLen);
            }
            Op::RefNew => {
                let cell = self.cell_of(self.var_ty(binds[0]))?;
                self.atoms(args);
                self.emit(I::StructNew(cell));
            }
            Op::RefGet | Op::RefSet => {
                let cell = self.cell_of(&self.atom_ty(&args[0]))?;
                self.atoms(args);
                self.emit(match op {
                    Op::RefGet => I::StructGet {
                        struct_type_index: cell,
                        field_index: 0,
                    },
                    _ => I::StructSet {
                        struct_type_index: cell,
                        field_index: 0,
                    },
                });
            }
            Op::Box(ty) => {
                self.atoms(args);
                let val = self.cx.types.val(ty)?;
                if is_scalar(val) {
                    let b = self.cx.types.boxed(val)?;
                    self.emit(I::StructNew(b));
                }
                // A reference is already an `anyref`.
            }
            Op::Unbox(ty) => {
                self.atoms(args);
                match self.cx.types.val(ty)? {
                    val if is_scalar(val) => {
                        let b = self.cx.types.boxed(val)?;
                        self.emit(I::RefCastNullable(HeapType::Concrete(b)));
                        self.emit(I::StructGet {
                            struct_type_index: b,
                            field_index: 0,
                        });
                    }
                    ValType::Ref(RefType { heap_type, .. }) if heap_type != HeapType::ANY => {
                        self.emit(I::RefCastNullable(heap_type));
                    }
                    _ => {}
                }
            }
            Op::Copy(_) => return Err("copy isn't supported yet".into()),
            Op::Handle(_) | Op::Perform(..) | Op::Resume => {
                return Err(
                    "effect handlers aren't supported yet: lower them with evidence passing first"
                        .into(),
                );
            }
        }
        Ok(self.bind(binds, false))
    }

    /// Stores an op's results, unless it was a tail call. Returns `tail`.
    fn bind(&mut self, binds: &[Var], tail: bool) -> bool {
        if !tail {
            for &b in binds.iter().rev() {
                self.emit(I::LocalSet(self.local(b)));
            }
        }
        tail
    }

    /// Whether a call bound by `binds` and followed by `body` can be a tail
    /// call: `body` returns exactly what the call gives.
    fn is_tail(&mut self, binds: &[Var], body: &Term) -> Result<bool, String> {
        let Term::Jump { target, args, .. } = body else {
            return Ok(false);
        };
        if *target != Label::RET
            || args.len() != binds.len()
            || !args.iter().zip(binds).all(|(a, b)| *a == Atom::Var(*b))
        {
            return Ok(false);
        }
        let mut tys = Vec::with_capacity(binds.len());
        for &b in binds {
            tys.push(self.cx.types.val(self.var_ty(b))?);
        }
        Ok(tys == self.results)
    }

    /// Pushes the object of a `Get` or `Set`, cast to its constructor if
    /// the ADT has several.
    fn object(&mut self, c: CtorId, obj: &Atom) -> Result<(u32, u32), String> {
        let (idx, first) = self.cx.types.ctor(c)?;
        self.atom(obj);
        if first == 1 {
            self.emit(I::RefCastNullable(HeapType::Concrete(idx)));
        }
        Ok((idx, first))
    }

    fn array_of(&mut self, ty: &Ty) -> Result<u32, String> {
        match ty {
            Ty::Array(elem, _) => self.cx.types.array(elem),
            _ => Err(format!("expected an array, but got a {ty}")),
        }
    }

    fn cell_of(&mut self, ty: &Ty) -> Result<u32, String> {
        match ty {
            Ty::Ref(inner) => self.cx.types.cell(inner),
            _ => Err(format!("expected a ref, but got a {ty}")),
        }
    }

    fn prim(&mut self, p: &Prim, args: &[Atom]) -> Result<(), String> {
        let Some(first) = args.first() else {
            return Err(format!("{p:?} needs an argument"));
        };
        let ty = self.atom_ty(first);
        if let (Prim::Neg, Ty::I32 | Ty::I64) = (p, &ty) {
            // Wasm has no integer negation: it's `0 - x`.
            self.emit(if ty == Ty::I32 {
                I::I32Const(0)
            } else {
                I::I64Const(0)
            });
            self.atom(first);
            self.emit(if ty == Ty::I32 { I::I32Sub } else { I::I64Sub });
            return Ok(());
        }
        self.atoms(args);
        match prim_instr(p, &ty) {
            Some(Some(i)) => self.emit(i),
            Some(None) => {}
            None => return Err(format!("{p:?} isn't defined on {ty}")),
        }
        Ok(())
    }
}

fn is_scalar(v: ValType) -> bool {
    matches!(v, ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64)
}

/// The instruction for a scalar op whose first operand has type `ty`.
/// `Some(None)` means no instruction is needed, as for a conversion to the
/// same type.
fn prim_instr(p: &Prim, ty: &Ty) -> Option<Option<I<'static>>> {
    use Sign::{Signed as S, Unsigned as U};
    use Ty::{F32, F64, I32, I64};

    // Picks the instruction for each operand type.
    macro_rules! by_ty {
        ($($t:ident => $i:expr),* $(,)?) => {
            match ty {
                $($t => Some($i),)*
                #[allow(unreachable_patterns)]
                _ => None,
            }
        };
    }
    macro_rules! by_sign {
        ($s:expr, $signed:expr, $unsigned:expr) => {
            match $s {
                S => $signed,
                U => $unsigned,
            }
        };
    }

    let i = match p {
        Prim::Add => by_ty!(I32 => I::I32Add, I64 => I::I64Add, F32 => I::F32Add, F64 => I::F64Add),
        Prim::Sub => by_ty!(I32 => I::I32Sub, I64 => I::I64Sub, F32 => I::F32Sub, F64 => I::F64Sub),
        Prim::Mul => by_ty!(I32 => I::I32Mul, I64 => I::I64Mul, F32 => I::F32Mul, F64 => I::F64Mul),
        Prim::Div(s) => by_ty!(
            I32 => by_sign!(s, I::I32DivS, I::I32DivU),
            I64 => by_sign!(s, I::I64DivS, I::I64DivU),
            F32 => I::F32Div,
            F64 => I::F64Div,
        ),
        Prim::Rem(s) => by_ty!(
            I32 => by_sign!(s, I::I32RemS, I::I32RemU),
            I64 => by_sign!(s, I::I64RemS, I::I64RemU),
        ),
        Prim::Neg => by_ty!(F32 => I::F32Neg, F64 => I::F64Neg),
        Prim::And => by_ty!(I32 => I::I32And, I64 => I::I64And),
        Prim::Or => by_ty!(I32 => I::I32Or, I64 => I::I64Or),
        Prim::Xor => by_ty!(I32 => I::I32Xor, I64 => I::I64Xor),
        Prim::Shl => by_ty!(I32 => I::I32Shl, I64 => I::I64Shl),
        Prim::Shr(s) => by_ty!(
            I32 => by_sign!(s, I::I32ShrS, I::I32ShrU),
            I64 => by_sign!(s, I::I64ShrS, I::I64ShrU),
        ),
        Prim::Eq => by_ty!(I32 => I::I32Eq, I64 => I::I64Eq, F32 => I::F32Eq, F64 => I::F64Eq),
        Prim::Ne => by_ty!(I32 => I::I32Ne, I64 => I::I64Ne, F32 => I::F32Ne, F64 => I::F64Ne),
        Prim::Lt(s) => by_ty!(
            I32 => by_sign!(s, I::I32LtS, I::I32LtU),
            I64 => by_sign!(s, I::I64LtS, I::I64LtU),
            F32 => I::F32Lt,
            F64 => I::F64Lt,
        ),
        Prim::Le(s) => by_ty!(
            I32 => by_sign!(s, I::I32LeS, I::I32LeU),
            I64 => by_sign!(s, I::I64LeS, I::I64LeU),
            F32 => I::F32Le,
            F64 => I::F64Le,
        ),
        Prim::Gt(s) => by_ty!(
            I32 => by_sign!(s, I::I32GtS, I::I32GtU),
            I64 => by_sign!(s, I::I64GtS, I::I64GtU),
            F32 => I::F32Gt,
            F64 => I::F64Gt,
        ),
        Prim::Ge(s) => by_ty!(
            I32 => by_sign!(s, I::I32GeS, I::I32GeU),
            I64 => by_sign!(s, I::I64GeS, I::I64GeU),
            F32 => I::F32Ge,
            F64 => I::F64Ge,
        ),
        Prim::Abs => by_ty!(F32 => I::F32Abs, F64 => I::F64Abs),
        Prim::Sqrt => by_ty!(F32 => I::F32Sqrt, F64 => I::F64Sqrt),
        Prim::Min => by_ty!(F32 => I::F32Min, F64 => I::F64Min),
        Prim::Max => by_ty!(F32 => I::F32Max, F64 => I::F64Max),
        Prim::Convert(to, s) => {
            return Some(Some(match (ty, to) {
                (a, b) if a == b => return Some(None),
                (I32, I64) => by_sign!(s, I::I64ExtendI32S, I::I64ExtendI32U),
                (I64, I32) => I::I32WrapI64,
                (I32, F32) => by_sign!(s, I::F32ConvertI32S, I::F32ConvertI32U),
                (I32, F64) => by_sign!(s, I::F64ConvertI32S, I::F64ConvertI32U),
                (I64, F32) => by_sign!(s, I::F32ConvertI64S, I::F32ConvertI64U),
                (I64, F64) => by_sign!(s, I::F64ConvertI64S, I::F64ConvertI64U),
                (F32, I32) => by_sign!(s, I::I32TruncF32S, I::I32TruncF32U),
                (F32, I64) => by_sign!(s, I::I64TruncF32S, I::I64TruncF32U),
                (F64, I32) => by_sign!(s, I::I32TruncF64S, I::I32TruncF64U),
                (F64, I64) => by_sign!(s, I::I64TruncF64S, I::I64TruncF64U),
                (F32, F64) => I::F64PromoteF32,
                (F64, F32) => I::F32DemoteF64,
                _ => return None,
            }));
        }
        Prim::Reinterpret(to) => match (ty, to) {
            (I32, F32) => Some(I::F32ReinterpretI32),
            (F32, I32) => Some(I::I32ReinterpretF32),
            (I64, F64) => Some(I::F64ReinterpretI64),
            (F64, I64) => Some(I::I64ReinterpretF64),
            _ => None,
        },
    };
    i.map(Some)
}
