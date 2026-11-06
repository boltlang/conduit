//! How a function body becomes Wasm code. See "Lowering" in `docs/ir.md`
//! and the crate's documentation for the layouts of objects.
//!
//! Every variable is a local, and every block ends in a `return`, `br`,
//! `return_call` or `unreachable`, so blocks never carry values and no
//! code falls through. Join points become blocks, and loops for recursive
//! ones. An `i8` or `i16` is an `i32` local whose low bits are the value,
//! as in LLVM's Wasm backend: the ops that read the high bits extend it
//! first, and memory stores only the low bits. Objects are loads and
//! stores at the offsets of their layouts.

use std::borrow::Cow;

use conduit_ir::{
    Atom, Block, CtorId, Func, FuncId, JoinDef, Label, Lit, Op, Prim, Sign, Tag, Tail, Ty, Var,
};
use wasm_encoder::{BlockType, Ieee32, Ieee64, Instruction as I, MemArg, MemArgTarget, ValType};

use crate::counts::{DropKey, Runtime, key_of};
use crate::{ABORT, Cx, HANDLERS, HEAP, SITE, Storage, layout, val};

/// A lowered function: the locals after its parameters, and its code.
pub(crate) struct Body {
    pub locals: Vec<ValType>,
    pub code: Vec<I<'static>>,
}

/// Where a jump to a label goes.
#[derive(Clone)]
struct Target {
    level: u32,
    params: Vec<Var>,
    which: Option<(u32, i32)>,
}

struct FuncCx<'a, 'm> {
    cx: &'a mut Cx<'m>,
    f: &'m Func,
    locals: Vec<u32>,
    extra: Vec<ValType>,
    results: Vec<ValType>,
    labels: Vec<Option<Target>>,
    depth: u32,
    code: Vec<I<'static>>,
}

/// Case arms on a dense range of `i32` tags dispatch with `br_table` when
/// there are at least this many.
const MIN_TABLE_ARMS: usize = 4;

/// The largest array, in bytes of elements, so that sizes stay in `i32`.
const MAX_BYTES: i32 = 0x7fff_0000;

/// The memory argument of a load or store of `s` at `offset`.
fn mem(offset: u32, s: Storage) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: s.size().trailing_zeros(),
        target: MemArgTarget::Memory(0),
        ordering: None,
    }
}

pub(crate) fn load(s: Storage, offset: u32) -> I<'static> {
    let m = mem(offset, s);
    match s {
        Storage::I8 => I::I32Load8S(m),
        Storage::I16 => I::I32Load16S(m),
        Storage::I32 => I::I32Load(m),
        Storage::I64 => I::I64Load(m),
        Storage::F32 => I::F32Load(m),
        Storage::F64 => I::F64Load(m),
    }
}

pub(crate) fn store(s: Storage, offset: u32) -> I<'static> {
    let m = mem(offset, s);
    match s {
        Storage::I8 => I::I32Store8(m),
        Storage::I16 => I::I32Store16(m),
        Storage::I32 => I::I32Store(m),
        Storage::I64 => I::I64Store(m),
        Storage::F32 => I::F32Store(m),
        Storage::F64 => I::F64Store(m),
    }
}

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
            extra.push(val(ty));
        }
    }
    let results = f.results.iter().map(val).collect();
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
    fcx.block(&f.body)?;
    fcx.code.push(I::End);
    Ok(Body {
        locals: fcx.extra,
        code: fcx.code,
    })
}

/// The wrapper closures of `f` with `n` captures call: it reads the
/// captures out of the closure and tail-calls `f`.
pub(crate) fn lower_entry(cx: &mut Cx<'_>, f: FuncId, n: usize) -> Result<Body, String> {
    let func = &cx.m.funcs[&f];
    let sig = func.sig();
    let (offsets, _) = layout(&sig.params[..n]);
    let rest = sig.params.len() - n;
    let mut code = Vec::new();
    let mut locals = Vec::new();
    if let Some(rt) = &cx.rt {
        // `apply` takes the closure, and the function owns its captures:
        // they're read out, each dupped, and the closure dropped.
        let first = 1 + rest as u32;
        for (j, (&(at, s), ty)) in offsets.iter().zip(&sig.params[..n]).enumerate() {
            locals.push(val(ty));
            code.extend([I::LocalGet(0), load(s, at), I::LocalTee(first + j as u32)]);
            match key_of(ty) {
                Some(_) => code.push(I::Call(rt.dup())),
                None => code.push(I::Drop),
            }
        }
        code.extend([I::LocalGet(0), I::Call(rt.drop(&DropKey::Closure))]);
        for j in 0..n as u32 {
            code.push(I::LocalGet(first + j));
        }
    } else {
        for &(at, s) in &offsets {
            code.push(I::LocalGet(0));
            code.push(load(s, at));
        }
    }
    for p in 0..rest as u32 {
        code.push(I::LocalGet(1 + p));
    }
    code.push(I::ReturnCall(cx.func_index(f)?));
    code.push(I::End);
    Ok(Body { locals, code })
}

/// The wrapper of clause `i` of `h`, which `perform` calls with the frame
/// and the operation's parameters: it reads the environment out of the
/// frame and calls the clause, as a plain function. For an aborting
/// clause, it stores what the clause gives in the frame, and throws.
pub(crate) fn lower_wrapper(
    cx: &mut Cx<'_>,
    h: conduit_ir::HandlerId,
    i: u32,
    plain: &Func,
) -> Result<Body, String> {
    let decl = &cx.m.handlers[&h];
    let clause = decl.ops[i as usize];
    let op = &cx.m.effects[&decl.effect].ops[i as usize];
    let params = op.params.len() as u32;
    let (fields, _) = cx.frame(h)?;
    let n_ops = decl.ops.len();
    let env = &fields[2 + n_ops..2 + n_ops + decl.env.len()];
    let mut code = Vec::new();
    for (&(at, s), ty) in env.iter().zip(&decl.env) {
        code.push(I::LocalGet(0));
        code.push(load(s, at));
        // The handler keeps its environment, and the clause gets its own.
        if let (Some(rt), Some(_)) = (&cx.rt, key_of(ty)) {
            code.extend([I::LocalGet(0), load(s, at), I::Call(rt.dup())]);
        }
    }
    for p in 0..params {
        code.push(I::LocalGet(1 + p));
    }
    let idx = cx.func_index(clause)?;
    let mut locals = Vec::new();
    if cx.is_abort(clause) {
        code.push(I::Call(idx));
        let results: Vec<ValType> = plain.results.iter().map(val).collect();
        let first = 1 + params;
        for (j, &ty) in results.iter().enumerate().rev() {
            let _ = ty;
            code.push(I::LocalSet(first + j as u32));
        }
        let outs = &fields[2 + n_ops + decl.env.len()..];
        for (j, &(at, s)) in outs.iter().enumerate() {
            code.push(I::LocalGet(0));
            code.push(I::LocalGet(first + j as u32));
            code.push(store(s, at));
        }
        locals = results;
        code.push(I::LocalGet(0));
        code.push(I::Throw(ABORT));
    } else {
        code.push(I::ReturnCall(idx));
    }
    code.push(I::End);
    Ok(Body { locals, code })
}

/// `alloc(size) -> address`: bumps the heap pointer by `size`, rounded up
/// to 8, growing the memory as needed. Growing past what the engine allows
/// traps.
pub(crate) fn lower_alloc() -> Body {
    let (size, p, end) = (0, 1, 2);
    let code = vec![
        I::LocalGet(size),
        I::I32Const(7),
        I::I32Add,
        I::I32Const(-8),
        I::I32And,
        I::LocalSet(size),
        I::GlobalGet(HEAP),
        I::LocalTee(p),
        I::LocalGet(size),
        I::I32Add,
        I::LocalTee(end),
        // An `end` below `p` wrapped around.
        I::LocalGet(p),
        I::I32LtU,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::LocalGet(end),
        I::MemorySize(0),
        I::I32Const(16),
        I::I32Shl,
        I::I32GtU,
        I::If(BlockType::Empty),
        I::LocalGet(end),
        I::MemorySize(0),
        I::I32Const(16),
        I::I32Shl,
        I::I32Sub,
        I::I32Const(65535),
        I::I32Add,
        I::I32Const(16),
        I::I32ShrU,
        I::MemoryGrow(0),
        I::I32Const(-1),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::End,
        I::LocalGet(end),
        I::GlobalSet(HEAP),
        I::LocalGet(p),
        I::End,
    ];
    Body {
        locals: vec![ValType::I32, ValType::I32],
        code,
    }
}

impl<'m> FuncCx<'_, 'm> {
    fn emit(&mut self, i: I<'static>) {
        self.code.push(i);
    }

    /// Calls a function of reference counting, if the module counts.
    fn call_rt(&mut self, f: impl Fn(&Runtime) -> u32) -> bool {
        match &self.cx.rt {
            Some(rt) => {
                let i = f(rt);
                self.emit(I::Call(i));
                true
            }
            None => false,
        }
    }

    /// How a value of type `ty` is dropped, if the module counts and it's
    /// counted.
    fn counted(&self, ty: &Ty) -> Option<DropKey> {
        self.cx.rt.as_ref().and(key_of(ty))
    }

    /// Drops the reference in local `v`, of kind `k`.
    fn drop_local(&mut self, v: u32, k: &DropKey) {
        self.emit(I::LocalGet(v));
        self.call_rt(|rt| rt.drop(k));
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
            Atom::Lit(Lit::I8(n)) => I::I32Const(n.into()),
            Atom::Lit(Lit::I16(n)) => I::I32Const(n.into()),
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

    fn block(&mut self, b: &'m Block) -> Result<(), String> {
        for (i, l) in b.lets.iter().enumerate() {
            if self.let_(&l.binds, &l.op, &l.args, b, i + 1)? {
                return Ok(());
            }
        }
        match &b.tail {
            Tail::Join { rec, defs, body } => self.join(*rec, defs, body),
            Tail::Jump { target, args, .. } => self.jump(*target, args),
            Tail::Case {
                scrut,
                arms,
                default,
            } => self.case(scrut, arms, default.as_deref()),
            // Wasm's `unreachable` traps, which is what `trap` does, and
            // one of the things undefined behavior can do.
            Tail::Unreachable | Tail::Trap => {
                self.emit(I::Unreachable);
                Ok(())
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

    fn join(&mut self, rec: bool, defs: &'m [JoinDef], body: &'m Block) -> Result<(), String> {
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
            self.block(body)?;
            for def in defs {
                self.close();
                self.block(&def.body)?;
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
        self.block(body)?;
        self.close();
        self.open(I::Loop(BlockType::Empty));
        match which {
            None => self.block(&defs[0].body)?,
            Some(which) => {
                for _ in defs {
                    self.open(I::Block(BlockType::Empty));
                }
                self.emit(I::LocalGet(which));
                let targets: Vec<u32> = (0..defs.len() as u32).collect();
                self.emit(I::BrTable(Cow::Owned(targets), defs.len() as u32 - 1));
                for def in defs {
                    self.close();
                    self.block(&def.body)?;
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
        default: Option<&'m Block>,
    ) -> Result<(), String> {
        let ty = self.atom_ty(scrut);
        let data = match ty {
            Ty::Data(d, ..) => Some(d),
            _ => None,
        };
        // A value of an ADT with one constructor can only be that one.
        let only = match data {
            Some(d)
                if self
                    .cx
                    .m
                    .data
                    .get(&d)
                    .is_some_and(|decl| decl.ctors.len() == 1) =>
            {
                Some(arms.first().map(|a| &a.body).or(default))
            }
            _ if arms.is_empty() => Some(default),
            _ => None,
        };
        if let Some(only) = only {
            return match only {
                Some(t) => self.block(t),
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
                let ctors = self.cx.m.data[&d].ctors.len() as u32;
                let table = (0..ctors)
                    .map(|index| {
                        let tag = Tag::Ctor(CtorId { data: d, index });
                        arms.iter()
                            .position(|a| a.tag == tag)
                            .map_or(fallback, |i| i as u32)
                    })
                    .collect();
                // The constructor's index is at 4.
                self.atom(scrut);
                self.emit(I::I32Load(mem(4, Storage::I32)));
                self.emit(I::BrTable(Cow::Owned(table), fallback));
            }
            None => self.int_dispatch(scrut, &ty, arms, default.is_some())?,
        }
        for arm in arms {
            self.close();
            self.block(&arm.body)?;
        }
        if let Some(default) = default {
            self.close();
            self.block(default)?;
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
        let small = *ty != Ty::I64;
        let dense = small && n >= MIN_TABLE_ARMS && max - min < 2 * n as i64;
        if dense && has_default {
            let table = (min..=max)
                .map(|v| tags.iter().position(|&t| t == v).unwrap_or(n) as u32)
                .collect();
            self.int(scrut, ty, Sign::Signed);
            if min != 0 {
                self.emit(I::I32Const(min as i32));
                self.emit(I::I32Sub);
            }
            self.emit(I::BrTable(Cow::Owned(table), n as u32));
            return Ok(());
        }
        for (i, &tag) in tags.iter().enumerate() {
            self.int(scrut, ty, Sign::Signed);
            match ty {
                Ty::I8 | Ty::I16 | Ty::I32 => {
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

    /// Lowers a `let`, which `next` follows in `block`. Returns whether it
    /// was a tail call, which also covers the `jump ret` after it.
    fn let_(
        &mut self,
        binds: &[Var],
        op: &'m Op,
        args: &[Atom],
        block: &Block,
        next: usize,
    ) -> Result<bool, String> {
        let tail = self.is_tail(binds, block, next)?;
        match op {
            Op::Prim(p) => self.prim(p, args)?,
            Op::Call(f, ..) => {
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
            Op::Closure(f, ..) => {
                let slot = self.cx.entry(*f, args.len());
                let tys: Vec<Ty> = args.iter().map(|a| self.atom_ty(a)).collect();
                let (offsets, size) = layout(&tys);
                let p = self.alloc_const(size);
                self.emit(I::LocalGet(p));
                self.emit(I::I32Const(slot as i32));
                self.emit(store(Storage::I32, 4));
                self.store_all(p, &offsets, args);
                self.emit(I::LocalGet(p));
            }
            Op::Apply => {
                let Some((c, rest)) = args.split_first() else {
                    return Err("apply needs a closure".into());
                };
                let Ty::Fn(sig, _) = self.atom_ty(c) else {
                    return Err("apply of a value that isn't a closure".into());
                };
                let ty = self.cx.entry_sig(&sig);
                self.atom(c);
                self.atoms(rest);
                self.atom(c);
                self.emit(load(Storage::I32, 4));
                self.emit(if tail {
                    I::ReturnCallIndirect {
                        type_index: ty,
                        table_index: 0,
                    }
                } else {
                    I::CallIndirect {
                        type_index: ty,
                        table_index: 0,
                    }
                });
                return Ok(self.bind(binds, tail));
            }
            Op::New(c) => {
                let (offsets, size) = layout(self.cx.fields(*c)?);
                let p = self.alloc_const(size);
                self.emit(I::LocalGet(p));
                self.emit(I::I32Const(c.index as i32));
                self.emit(store(Storage::I32, 4));
                self.store_all(p, &offsets, args);
                self.emit(I::LocalGet(p));
            }
            Op::Get(c, i) => {
                let (at, s) = self.field(*c, *i)?;
                self.atom(&args[0]);
                self.emit(load(s, at));
            }
            Op::Set(c, i) => {
                let (at, s) = self.field(*c, *i)?;
                let field = self.cx.fields(*c)?[*i as usize].clone();
                let old = self.counted(&field).map(|k| {
                    let old = self.new_local(ValType::I32);
                    self.atom(&args[0]);
                    self.emit(load(s, at));
                    self.emit(I::LocalSet(old));
                    (old, k)
                });
                self.atom(&args[0]);
                self.atom(&args[1]);
                self.emit(store(s, at));
                // It drops the value it replaces.
                if let Some((old, k)) = old {
                    self.drop_local(old, &k);
                }
            }
            Op::Const(id) => match self.cx.constant(*id)? {
                Ok(addr) => self.emit(I::I32Const(addr as i32)),
                Err(lit) => self.atom(&Atom::Lit(lit)),
            },
            // 0 is no object, and every value has it.
            Op::Undefined => {
                let ty = val(self.var_ty(binds[0]));
                self.emit(zero(ty));
            }
            Op::ArrayNew => {
                let s = self.elem_of(self.var_ty(binds[0]))?;
                let size = (8 + s.size() * args.len() as u32).next_multiple_of(8);
                let p = self.alloc_const(size);
                self.emit(I::LocalGet(p));
                self.emit(I::I32Const(args.len() as i32));
                self.emit(store(Storage::I32, 4));
                for (i, a) in args.iter().enumerate() {
                    self.emit(I::LocalGet(p));
                    self.atom(a);
                    self.emit(store(s, 8 + s.size() * i as u32));
                }
                self.emit(I::LocalGet(p));
            }
            Op::ArrayFill | Op::ArrayAlloc => {
                let s = self.elem_of(self.var_ty(binds[0]))?;
                let n = self.new_local(ValType::I32);
                self.atom(&args[0]);
                self.emit(I::LocalSet(n));
                let p = self.alloc_array(n, s);
                let elem = match self.var_ty(binds[0]) {
                    Ty::Array(e, _) => self.counted(e),
                    _ => None,
                };
                if *op == Op::ArrayFill
                    && let Some(k) = elem
                {
                    // It holds its value once per element: it takes one
                    // reference and adds the rest, or drops it for none.
                    self.emit(I::LocalGet(n));
                    self.emit(I::I32Eqz);
                    self.open(I::If(BlockType::Empty));
                    self.atom(&args[1]);
                    self.call_rt(|rt| rt.drop(&k));
                    self.emit(I::Else);
                    self.atom(&args[1]);
                    self.emit(I::LocalGet(n));
                    self.emit(I::I32Const(1));
                    self.emit(I::I32Sub);
                    self.call_rt(|rt| rt.add());
                    self.close();
                }
                if *op == Op::ArrayFill {
                    // Every element, from the last.
                    self.open(I::Block(BlockType::Empty));
                    self.open(I::Loop(BlockType::Empty));
                    self.emit(I::LocalGet(n));
                    self.emit(I::I32Eqz);
                    self.emit(I::BrIf(1));
                    self.emit(I::LocalGet(n));
                    self.emit(I::I32Const(1));
                    self.emit(I::I32Sub);
                    self.emit(I::LocalTee(n));
                    self.elem_addr_of(p, s);
                    self.atom(&args[1]);
                    self.emit(store(s, 8));
                    self.emit(I::Br(0));
                    self.close();
                    self.close();
                }
                self.emit(I::LocalGet(p));
            }
            Op::ArrayGet => {
                let s = self.elem_of(&self.atom_ty(&args[0]))?;
                self.elem_addr(&args[0], &args[1], s);
                self.emit(load(s, 8));
            }
            Op::ArraySet => {
                let ty = self.atom_ty(&args[0]);
                let s = self.elem_of(&ty)?;
                let elem = match &ty {
                    Ty::Array(e, _) => self.counted(e),
                    _ => None,
                };
                self.elem_addr(&args[0], &args[1], s);
                match elem {
                    // It drops the element it replaces.
                    Some(k) => {
                        let (at, old) =
                            (self.new_local(ValType::I32), self.new_local(ValType::I32));
                        self.emit(I::LocalTee(at));
                        self.emit(load(s, 8));
                        self.emit(I::LocalSet(old));
                        self.emit(I::LocalGet(at));
                        self.atom(&args[2]);
                        self.emit(store(s, 8));
                        self.drop_local(old, &k);
                    }
                    None => {
                        self.atom(&args[2]);
                        self.emit(store(s, 8));
                    }
                }
            }
            Op::ArrayLen => {
                self.atom(&args[0]);
                self.emit(load(Storage::I32, 4));
            }
            Op::ArrayClone => {
                let s = self.elem_of(&self.atom_ty(&args[0]))?;
                let n = self.new_local(ValType::I32);
                self.atom(&args[0]);
                self.emit(load(Storage::I32, 4));
                self.emit(I::LocalSet(n));
                let p = self.alloc_array(n, s);
                self.emit(I::LocalGet(p));
                self.emit(I::I32Const(8));
                self.emit(I::I32Add);
                self.atom(&args[0]);
                self.emit(I::I32Const(8));
                self.emit(I::I32Add);
                self.emit(I::LocalGet(n));
                self.scale(s);
                self.emit(I::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                // The new array shares the elements.
                if let Ty::Array(e, _) = self.atom_ty(&args[0])
                    && self.counted(&e).is_some()
                {
                    self.emit(I::LocalGet(p));
                    self.emit(I::I32Const(8));
                    self.emit(I::I32Add);
                    self.emit(I::LocalGet(n));
                    self.call_rt(|rt| rt.dup_range());
                }
                self.emit(I::LocalGet(p));
            }
            // Mutability doesn't exist at run time.
            Op::ArrayFreeze => self.atoms(args),
            // Without counting, nothing is freed, and no object is unique,
            // so code that specialized a `drop` takes the branch that only
            // decrements.
            Op::Dup | Op::Drop | Op::Free | Op::DecRef => {
                if let Some(k) = self.counted(&self.atom_ty(&args[0])) {
                    self.atom(&args[0]);
                    self.call_rt(|rt| match op {
                        Op::Dup => rt.dup(),
                        Op::Drop => rt.drop(&k),
                        Op::Free => rt.free(&k),
                        _ => rt.decref(),
                    });
                }
            }
            Op::IsUnique => {
                self.atom(&args[0]);
                if !self.call_rt(|rt| rt.is_unique()) {
                    self.emit(I::Drop);
                    self.emit(I::I32Const(0));
                }
            }
            Op::ArrayCopy => {
                let s = self.elem_of(&self.atom_ty(&args[0]))?;
                // Both ranges must fit, their starts and count read as
                // unsigned, as Wasm's `array.copy` does.
                for (array, start) in [(&args[0], &args[1]), (&args[2], &args[3])] {
                    self.atom(start);
                    self.emit(I::I64ExtendI32U);
                    self.atom(&args[4]);
                    self.emit(I::I64ExtendI32U);
                    self.emit(I::I64Add);
                    self.atom(array);
                    self.emit(load(Storage::I32, 4));
                    self.emit(I::I64ExtendI32U);
                    self.emit(I::I64GtU);
                    self.trap_if();
                }
                let mut addrs = Vec::new();
                for (array, start) in [(&args[0], &args[1]), (&args[2], &args[3])] {
                    self.atom(array);
                    self.emit(I::I32Const(8));
                    self.emit(I::I32Add);
                    self.atom(start);
                    self.scale(s);
                    self.emit(I::I32Add);
                    let a = self.new_local(ValType::I32);
                    self.emit(I::LocalSet(a));
                    addrs.push(a);
                }
                // It dups what it copies, then drops what it overwrites.
                if let Ty::Array(e, _) = self.atom_ty(&args[0])
                    && let Some(k) = self.counted(&e)
                {
                    self.emit(I::LocalGet(addrs[1]));
                    self.atom(&args[4]);
                    self.call_rt(|rt| rt.dup_range());
                    self.emit(I::LocalGet(addrs[0]));
                    self.atom(&args[4]);
                    self.call_rt(|rt| rt.drop_range(&k));
                }
                self.emit(I::LocalGet(addrs[0]));
                self.emit(I::LocalGet(addrs[1]));
                self.atom(&args[4]);
                self.scale(s);
                self.emit(I::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            Op::RefNew => {
                let s = Storage::of(&self.atom_ty(&args[0]));
                let p = self.alloc_const(16);
                self.emit(I::LocalGet(p));
                self.atom(&args[0]);
                self.emit(store(s, 8));
                self.emit(I::LocalGet(p));
            }
            Op::RefGet => {
                let Ty::Ref(inner) = self.atom_ty(&args[0]) else {
                    return Err("ref get of something that isn't a ref".into());
                };
                self.atom(&args[0]);
                self.emit(load(Storage::of(&inner), 8));
            }
            Op::RefSet => {
                let Ty::Ref(inner) = self.atom_ty(&args[0]) else {
                    return Err("ref set of something that isn't a ref".into());
                };
                let s = Storage::of(&inner);
                let old = self.counted(&inner).map(|k| {
                    let old = self.new_local(ValType::I32);
                    self.atom(&args[0]);
                    self.emit(load(s, 8));
                    self.emit(I::LocalSet(old));
                    (old, k)
                });
                self.atom(&args[0]);
                self.atom(&args[1]);
                self.emit(store(s, 8));
                if let Some((old, k)) = old {
                    self.drop_local(old, &k);
                }
            }
            // When counting, everything goes in a box, which holds the kind
            // of what it holds, plus 1, or 0 for a scalar, so that dropping
            // the box can drop it.
            Op::Box(ty) if self.cx.rt.is_some() => {
                let kind = key_of(ty).map_or(0, |k| self.cx.rt.as_ref().unwrap().number(&k) + 1);
                let p = self.alloc_const(16);
                self.emit(I::LocalGet(p));
                self.emit(I::I32Const(kind as i32));
                self.emit(store(Storage::I32, 4));
                self.emit(I::LocalGet(p));
                self.atom(&args[0]);
                self.emit(store(Storage::of(ty), 8));
                self.emit(I::LocalGet(p));
            }
            Op::Unbox(ty) if self.cx.rt.is_some() => {
                self.atom(&args[0]);
                self.emit(load(Storage::of(ty), 8));
            }
            // A reference is already an `any`; a scalar goes in a box.
            Op::Box(ty) => {
                if is_scalar_ty(ty) {
                    let p = self.alloc_const(16);
                    self.emit(I::LocalGet(p));
                    self.atom(&args[0]);
                    self.emit(store(Storage::of(ty), 8));
                    self.emit(I::LocalGet(p));
                } else {
                    self.atom(&args[0]);
                }
            }
            Op::Unbox(ty) => {
                self.atom(&args[0]);
                if is_scalar_ty(ty) {
                    self.emit(load(Storage::of(ty), 8));
                }
            }
            Op::Handle(h, _) => {
                let tail = self.handle(*h, args, tail)?;
                return Ok(self.bind(binds, tail));
            }
            Op::Perform(e, i) => self.perform(*e, *i, args)?,
            // A continuation is a closure: resuming it calls it.
            Op::Resume => {
                let Some((k, rest)) = args.split_first() else {
                    return Err("resume without a continuation".into());
                };
                let Ty::Cont(sig) = self.atom_ty(k) else {
                    return Err("resume of something that isn't a continuation".into());
                };
                let ty = self.cx.entry_sig(&sig);
                self.atom(k);
                self.atoms(rest);
                self.atom(k);
                self.emit(load(Storage::I32, 4));
                self.emit(if tail {
                    I::ReturnCallIndirect {
                        type_index: ty,
                        table_index: 0,
                    }
                } else {
                    I::CallIndirect {
                        type_index: ty,
                        table_index: 0,
                    }
                });
                return Ok(self.bind(binds, tail));
            }
            // A continuation's: throws a closure that performs the
            // operation into it.
            Op::Discontinue(e, i)
                if args
                    .first()
                    .is_some_and(|k| matches!(self.atom_ty(k), Ty::Cont(_))) =>
            {
                let (k, rest) = args.split_first().unwrap();
                let Ty::Cont(sig) = self.atom_ty(k) else {
                    unreachable!()
                };
                let thunk = self.cx.thunk(*e, *i)?;
                let slot = self.cx.entry(thunk, rest.len());
                let tys: Vec<Ty> = rest.iter().map(|a| self.atom_ty(a)).collect();
                let (offsets, size) = layout(&tys);
                let t = self.alloc_const(size);
                self.emit(I::LocalGet(t));
                self.emit(I::I32Const(slot as i32));
                self.emit(store(Storage::I32, 4));
                self.store_all(t, &offsets, rest);
                let results = sig.results.iter().map(val).collect();
                let ty = self.cx.disc_sig(results);
                self.atom(k);
                self.emit(I::LocalGet(t));
                self.atom(k);
                self.emit(load(Storage::I32, 8));
                self.emit(if tail {
                    I::ReturnCallIndirect {
                        type_index: ty,
                        table_index: 0,
                    }
                } else {
                    I::CallIndirect {
                        type_index: ty,
                        table_index: 0,
                    }
                });
                return Ok(self.bind(binds, tail));
            }
            // In a tail-resumptive clause, without its continuation: the
            // `perform` it handles performs this instead, under its
            // handlers.
            Op::Discontinue(e, i) => {
                self.emit(I::GlobalGet(SITE));
                self.emit(I::GlobalSet(HANDLERS));
                self.perform(*e, *i, args)?;
            }
        }
        Ok(self.bind(binds, false))
    }

    /// Gives a handler's frame back when counting, once its `handle` is
    /// done with it.
    fn release_frame(&mut self, frame: u32, size: u32) {
        if self.cx.rt.is_some() {
            self.emit(I::LocalGet(frame));
            self.emit(I::I32Const(size as i32));
            self.call_rt(|rt| rt.release());
        }
    }

    /// Calls the closure `body` with no arguments, leaving its results.
    fn apply_body(&mut self, body: &Atom) -> Result<(), String> {
        let Ty::Fn(sig, _) = self.atom_ty(body) else {
            return Err("the body of a handle isn't a closure".into());
        };
        let ty = self.cx.entry_sig(&sig);
        self.atom(body);
        self.atom(body);
        self.emit(load(Storage::I32, 4));
        self.emit(I::CallIndirect {
            type_index: ty,
            table_index: 0,
        });
        Ok(())
    }

    /// `handle h(env.., body)`: pushes a frame of `h`, runs the body, pops
    /// the frame and calls the return clause. If `h` has an aborting
    /// clause, the body runs in a `try_table` that catches the exception
    /// holding this frame, and the results are then the frame's. Returns
    /// whether it was a tail call.
    fn handle(
        &mut self,
        h: conduit_ir::HandlerId,
        args: &[Atom],
        tail: bool,
    ) -> Result<bool, String> {
        let m = self.cx.m;
        let decl = &m.handlers[&h];
        let (env, body) = args.split_at(args.len() - 1);
        let (fields, size) = self.cx.frame(h)?;
        let number = self.cx.effect_number(decl.effect)?;
        let ret = self.cx.func_index(decl.ret)?;
        let Ty::Fn(sig, _) = self.atom_ty(&body[0]) else {
            return Err(format!("the body of handle {h} isn't a closure"));
        };
        let results: Vec<u32> = sig.results.iter().map(|t| self.new_local(val(t))).collect();
        let n_ops = decl.ops.len();
        if self.cx.is_switching(h) {
            // A frame with -1 in every clause slot, which `start` links in
            // before it runs the body.
            let start = self.cx.switch(h)?.start;
            let frame = self.alloc_const(size);
            self.emit(I::LocalGet(frame));
            self.emit(I::I32Const(number));
            self.emit(store(Storage::I32, fields[0].0));
            for i in 0..n_ops {
                self.emit(I::LocalGet(frame));
                self.emit(I::I32Const(-1));
                self.emit(store(Storage::I32, fields[2 + i].0));
            }
            let env_fields = fields[2 + n_ops..2 + n_ops + env.len()].to_vec();
            self.store_all(frame, &env_fields, env);
            self.emit(I::LocalGet(frame));
            self.atoms(env);
            self.atom(&body[0]);
            self.emit(if tail {
                I::ReturnCall(start)
            } else {
                I::Call(start)
            });
            let _ = results;
            return Ok(tail);
        }
        let aborting = self.cx.is_aborting(h);
        let site = (aborting && self.cx.discontinues).then(|| {
            let site = self.new_local(ValType::I32);
            self.emit(I::GlobalGet(SITE));
            self.emit(I::LocalSet(site));
            site
        });

        let saved = self.new_local(ValType::I32);
        self.emit(I::GlobalGet(HANDLERS));
        self.emit(I::LocalSet(saved));
        let frame = self.alloc_const(size);
        self.emit(I::LocalGet(frame));
        self.emit(I::I32Const(number));
        self.emit(store(Storage::I32, fields[0].0));
        self.emit(I::LocalGet(frame));
        self.emit(I::LocalGet(saved));
        self.emit(store(Storage::I32, fields[1].0));
        for i in 0..n_ops {
            let slot = self.cx.wrapper(h, i as u32)?;
            self.emit(I::LocalGet(frame));
            self.emit(I::I32Const(slot as i32));
            self.emit(store(Storage::I32, fields[2 + i].0));
        }
        let env_fields = fields[2 + n_ops..2 + n_ops + env.len()].to_vec();
        self.store_all(frame, &env_fields, env);
        self.emit(I::LocalGet(frame));
        self.emit(I::GlobalSet(HANDLERS));

        if !aborting {
            if self.cx.rt.is_some() {
                // When counting, an abort to a handler further out that
                // unwinds through this one gives its frame back first.
                self.open(I::Block(BlockType::Empty));
                self.open(I::Block(BlockType::Result(ValType::Ref(
                    wasm_encoder::RefType::EXNREF,
                ))));
                let catches = vec![wasm_encoder::Catch::AllRef { label: 0 }];
                self.open(I::TryTable(BlockType::Empty, Cow::Owned(catches)));
                self.apply_body(&body[0])?;
                for &r in results.iter().rev() {
                    self.emit(I::LocalSet(r));
                }
                self.close();
                self.emit(I::Br(1));
                self.close();
                self.release_frame(frame, size);
                self.emit(I::ThrowRef);
                self.close();
            } else {
                self.apply_body(&body[0])?;
                for &r in results.iter().rev() {
                    self.emit(I::LocalSet(r));
                }
            }
            self.emit(I::LocalGet(saved));
            self.emit(I::GlobalSet(HANDLERS));
            self.release_frame(frame, size);
            self.atoms(env);
            for &r in &results {
                self.emit(I::LocalGet(r));
            }
            self.emit(if tail {
                I::ReturnCall(ret)
            } else {
                I::Call(ret)
            });
            return Ok(tail);
        }

        let ret_func = m
            .funcs
            .get(&decl.ret)
            .ok_or_else(|| format!("unknown return clause {}", decl.ret))?;
        let outs: Vec<u32> = ret_func
            .results
            .iter()
            .map(|t| self.new_local(val(t)))
            .collect();
        let thrown = self.new_local(ValType::I32);
        self.open(I::Block(BlockType::Empty));
        self.open(I::Block(BlockType::Result(ValType::I32)));
        // A catch's label is counted from outside the `try_table`.
        let catches = vec![wasm_encoder::Catch::One {
            tag: ABORT,
            label: 0,
        }];
        self.open(I::TryTable(BlockType::Empty, Cow::Owned(catches)));
        self.apply_body(&body[0])?;
        for &r in results.iter().rev() {
            self.emit(I::LocalSet(r));
        }
        self.close();
        // The body returned: run the return clause outside the handler,
        // done with the frame, since the clause can abort to a handler
        // further out.
        self.emit(I::LocalGet(saved));
        self.emit(I::GlobalSet(HANDLERS));
        self.release_frame(frame, size);
        self.atoms(env);
        for &r in &results {
            self.emit(I::LocalGet(r));
        }
        self.emit(I::Call(ret));
        for &o in outs.iter().rev() {
            self.emit(I::LocalSet(o));
        }
        self.emit(I::Br(1));
        self.close();
        // A clause aborted: to this handler, or else to one further out.
        self.emit(I::LocalTee(thrown));
        self.emit(I::LocalGet(frame));
        self.emit(I::I32Ne);
        self.open(I::If(BlockType::Empty));
        self.release_frame(frame, size);
        self.emit(I::LocalGet(thrown));
        self.emit(I::Throw(ABORT));
        self.close();
        self.emit(I::LocalGet(saved));
        self.emit(I::GlobalSet(HANDLERS));
        if let Some(site) = site {
            self.emit(I::LocalGet(site));
            self.emit(I::GlobalSet(SITE));
        }
        let out_fields = fields[2 + n_ops + env.len()..].to_vec();
        for (&o, &(at, s)) in outs.iter().zip(&out_fields) {
            self.emit(I::LocalGet(frame));
            self.emit(load(s, at));
            self.emit(I::LocalSet(o));
        }
        self.release_frame(frame, size);
        self.close();
        for &o in &outs {
            self.emit(I::LocalGet(o));
        }
        Ok(false)
    }

    /// `perform e.i(args)`: finds the nearest frame of `e`, trapping if
    /// there's none, and calls its clause with the handler stack below the
    /// frame, so that a `perform` in the clause goes to an outer handler.
    fn perform(&mut self, e: conduit_ir::EffectId, i: u32, args: &[Atom]) -> Result<(), String> {
        let number = self.cx.effect_number(e)?;
        let clause = self.cx.clause_sig(e, i)?;
        let saved = self.new_local(ValType::I32);
        let found = self.new_local(ValType::I32);
        self.emit(I::GlobalGet(HANDLERS));
        self.emit(I::LocalTee(saved));
        self.emit(I::LocalSet(found));
        self.open(I::Block(BlockType::Empty));
        self.open(I::Loop(BlockType::Empty));
        // No frame left: the effect isn't handled.
        self.emit(I::LocalGet(found));
        self.emit(I::I32Eqz);
        self.trap_if();
        self.emit(I::LocalGet(found));
        self.emit(load(Storage::I32, 8));
        self.emit(I::I32Const(number));
        self.emit(I::I32Eq);
        self.emit(I::BrIf(1));
        self.emit(I::LocalGet(found));
        self.emit(load(Storage::I32, 12));
        self.emit(I::LocalSet(found));
        self.emit(I::Br(0));
        self.close();
        self.close();

        if let Some(tag) = self.cx.tag(e, i) {
            return self.perform_or_suspend(e, i, args, saved, found, tag, clause);
        }
        self.call_clause(i, args, saved, found, clause);
        Ok(())
    }

    /// Calls clause `i` of the frame in local `found` with `args`, under
    /// the frames below it, and puts the handler stack in `saved` back.
    fn call_clause(&mut self, i: u32, args: &[Atom], saved: u32, found: u32, clause: u32) {
        self.emit(I::LocalGet(found));
        self.emit(load(Storage::I32, 12));
        self.emit(I::GlobalSet(HANDLERS));
        // The clause can discontinue under this `perform`'s handlers.
        let site = self.cx.discontinues.then(|| {
            let old = self.new_local(ValType::I32);
            self.emit(I::GlobalGet(SITE));
            self.emit(I::LocalSet(old));
            self.emit(I::LocalGet(saved));
            self.emit(I::GlobalSet(SITE));
            old
        });
        self.emit(I::LocalGet(found));
        self.atoms(args);
        self.emit(I::LocalGet(found));
        self.emit(load(Storage::I32, 16 + 4 * i));
        self.emit(I::CallIndirect {
            type_index: clause,
            table_index: 0,
        });
        self.emit(I::LocalGet(saved));
        self.emit(I::GlobalSet(HANDLERS));
        if let Some(old) = site {
            self.emit(I::LocalGet(old));
            self.emit(I::GlobalSet(SITE));
        }
    }

    /// `perform` of an effect some handler of which switches stacks: the
    /// frame found may be one, with -1 in its clause slots, which is
    /// suspended to, or else one whose clause is called. The results go
    /// through locals, as both ways give them.
    #[allow(clippy::too_many_arguments)]
    fn perform_or_suspend(
        &mut self,
        e: conduit_ir::EffectId,
        i: u32,
        args: &[Atom],
        saved: u32,
        found: u32,
        tag: u32,
        clause: u32,
    ) -> Result<(), String> {
        let m = self.cx.m;
        let op = &m.effects[&e].ops[i as usize];
        let results: Vec<u32> = op.results.iter().map(|t| self.new_local(val(t))).collect();
        self.emit(I::LocalGet(found));
        self.emit(load(Storage::I32, 16 + 4 * i));
        self.emit(I::I32Const(-1));
        self.emit(I::I32Eq);
        self.open(I::If(BlockType::Empty));
        match self.cx.disc_tag {
            None => {
                self.atoms(args);
                self.emit(I::Suspend(tag));
                for &r in results.iter().rev() {
                    self.emit(I::LocalSet(r));
                }
            }
            Some(disc) => {
                // A continuation discontinued here gets a closure thrown
                // at it, which performs what gives this instead.
                let thunk_sig = conduit_ir::Sig::new(Vec::new(), op.results.clone());
                let entry = self.cx.entry_sig(&thunk_sig);
                let t = self.new_local(ValType::I32);
                self.open(I::Block(BlockType::Empty));
                self.open(I::Block(BlockType::Result(ValType::I32)));
                let catches = vec![wasm_encoder::Catch::One {
                    tag: disc,
                    label: 0,
                }];
                self.open(I::TryTable(BlockType::Empty, Cow::Owned(catches)));
                self.atoms(args);
                self.emit(I::Suspend(tag));
                for &r in results.iter().rev() {
                    self.emit(I::LocalSet(r));
                }
                self.emit(I::Br(2));
                self.close();
                self.emit(I::Unreachable);
                self.close();
                self.emit(I::LocalSet(t));
                self.emit(I::LocalGet(saved));
                self.emit(I::GlobalSet(HANDLERS));
                self.emit(I::LocalGet(t));
                self.emit(I::LocalGet(t));
                self.emit(load(Storage::I32, 4));
                self.emit(I::CallIndirect {
                    type_index: entry,
                    table_index: 0,
                });
                for &r in results.iter().rev() {
                    self.emit(I::LocalSet(r));
                }
                self.close();
            }
        }
        // Resumed: this stack's handlers again.
        self.emit(I::LocalGet(saved));
        self.emit(I::GlobalSet(HANDLERS));
        self.emit(I::Else);
        self.call_clause(i, args, saved, found, clause);
        for &r in results.iter().rev() {
            self.emit(I::LocalSet(r));
        }
        self.close();
        for &r in &results {
            self.emit(I::LocalGet(r));
        }
        Ok(())
    }

    /// The offset and storage of field `i` of `c`.
    fn field(&self, c: CtorId, i: u32) -> Result<(u32, Storage), String> {
        let (offsets, _) = layout(self.cx.fields(c)?);
        offsets
            .get(i as usize)
            .copied()
            .ok_or_else(|| format!("{c} has no field {i}"))
    }

    /// How an array of type `ty` stores its elements.
    fn elem_of(&self, ty: &Ty) -> Result<Storage, String> {
        match ty {
            Ty::Array(elem, _) => Ok(Storage::of(elem)),
            _ => Err(format!("expected an array, but got a {ty}")),
        }
    }

    /// Allocates `size` bytes, into a new local.
    fn alloc_const(&mut self, size: u32) -> u32 {
        let p = self.new_local(ValType::I32);
        self.emit(I::I32Const(size as i32));
        self.emit(I::Call(self.cx.alloc));
        self.emit(I::LocalSet(p));
        p
    }

    /// Allocates an array of the length in local `n`, with its length set,
    /// into a new local. A negative or too big length traps.
    fn alloc_array(&mut self, n: u32, s: Storage) -> u32 {
        self.emit(I::LocalGet(n));
        self.emit(I::I32Const(MAX_BYTES / s.size() as i32));
        self.emit(I::I32GtU);
        self.trap_if();
        let p = self.new_local(ValType::I32);
        self.emit(I::LocalGet(n));
        self.scale(s);
        self.emit(I::I32Const(8));
        self.emit(I::I32Add);
        self.emit(I::Call(self.cx.alloc));
        self.emit(I::LocalTee(p));
        self.emit(I::LocalGet(n));
        self.emit(store(Storage::I32, 4));
        p
    }

    /// Multiplies the `i32` on the stack by the size of `s`.
    fn scale(&mut self, s: Storage) {
        let shift = s.size().trailing_zeros();
        if shift > 0 {
            self.emit(I::I32Const(shift as i32));
            self.emit(I::I32Shl);
        }
    }

    /// Pushes the address of element `index` of `array`, less 8, trapping
    /// if it's out of bounds.
    fn elem_addr(&mut self, array: &Atom, index: &Atom, s: Storage) {
        self.atom(index);
        self.atom(array);
        self.emit(load(Storage::I32, 4));
        self.emit(I::I32GeU);
        self.trap_if();
        self.atom(array);
        self.atom(index);
        self.scale(s);
        self.emit(I::I32Add);
    }

    /// With an index on the stack, pushes the address of that element of
    /// the array in local `p`, less 8.
    fn elem_addr_of(&mut self, p: u32, s: Storage) {
        self.scale(s);
        self.emit(I::LocalGet(p));
        self.emit(I::I32Add);
    }

    /// Stores `vals` at `offsets` of the object in local `p`.
    fn store_all(&mut self, p: u32, offsets: &[(u32, Storage)], vals: &[Atom]) {
        for (&(at, s), a) in offsets.iter().zip(vals) {
            self.emit(I::LocalGet(p));
            self.atom(a);
            self.emit(store(s, at));
        }
    }

    /// Traps if the `i32` on the stack isn't 0.
    fn trap_if(&mut self) {
        self.open(I::If(BlockType::Empty));
        self.emit(I::Unreachable);
        self.close();
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

    /// Whether a call bound by `binds`, followed by what's at `next` in
    /// `block`, can be a tail call: what follows returns exactly what the
    /// call gives.
    fn is_tail(&mut self, binds: &[Var], block: &Block, next: usize) -> Result<bool, String> {
        let Tail::Jump { target, args, .. } = &block.tail else {
            return Ok(false);
        };
        if next != block.lets.len() {
            return Ok(false);
        }
        if *target != Label::RET
            || args.len() != binds.len()
            || !args.iter().zip(binds).all(|(a, b)| *a == Atom::Var(*b))
        {
            return Ok(false);
        }
        let mut tys = Vec::with_capacity(binds.len());
        for &b in binds {
            tys.push(val(self.var_ty(b)));
        }
        Ok(tys == self.results)
    }

    /// Pushes an integer, with an `i8` or `i16` extended to an `i32` as
    /// `sign` says.
    fn int(&mut self, a: &Atom, ty: &Ty, sign: Sign) {
        self.atom(a);
        if let Some(bits) = narrow_bits(ty) {
            self.extend(bits, sign);
        }
    }

    /// Extends the low `bits` of the `i32` on the stack.
    fn extend(&mut self, bits: u32, sign: Sign) {
        match (sign, bits) {
            (Sign::Signed, 8) => self.emit(I::I32Extend8S),
            (Sign::Signed, _) => self.emit(I::I32Extend16S),
            (Sign::Unsigned, _) => {
                self.emit(I::I32Const((1 << bits) - 1));
                self.emit(I::I32And);
            }
        }
    }

    /// Traps unless the `i32` on the stack, read as `sign`, fits in `bits`,
    /// and leaves it there.
    fn check_fits(&mut self, bits: u32, sign: Sign) {
        let t = self.new_local(ValType::I32);
        self.emit(I::LocalTee(t));
        // Signed values in range are `-2^(bits-1) <= x < 2^(bits-1)`, so
        // adding `2^(bits-1)` puts them, like unsigned ones, below `2^bits`.
        if sign == Sign::Signed {
            self.emit(I::I32Const(1 << (bits - 1)));
            self.emit(I::I32Add);
        }
        self.emit(I::I32Const(1 << bits));
        self.emit(I::I32GeU);
        self.open(I::If(BlockType::Empty));
        self.emit(I::Unreachable);
        self.close();
        self.emit(I::LocalGet(t));
    }

    fn prim(&mut self, p: &Prim, args: &[Atom]) -> Result<(), String> {
        let Some(first) = args.first() else {
            return Err(format!("{p:?} needs an argument"));
        };
        let ty = self.atom_ty(first);
        if let (Prim::Div(_) | Prim::Rem(_), [x, y]) = (p, args)
            && matches!(ty, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64)
        {
            return self.div_or_rem(p, &ty, x, y);
        }
        self.prim_on(p, &ty, args)
    }

    /// Integer division or remainder, which Wasm traps on for a divisor of
    /// 0: then `x / 0` is 0 and `x % 0` is `x`, without the instruction.
    fn div_or_rem(&mut self, p: &Prim, ty: &Ty, x: &Atom, y: &Atom) -> Result<(), String> {
        let wide = *ty == Ty::I64;
        let on_zero = |cx: &mut Self| match p {
            Prim::Div(_) if wide => cx.emit(I::I64Const(0)),
            Prim::Div(_) => cx.emit(I::I32Const(0)),
            _ => cx.atom(x),
        };
        match y {
            Atom::Lit(l) => {
                let zero = match *l {
                    Lit::I8(n) => n == 0,
                    Lit::I16(n) => n == 0,
                    Lit::I32(n) => n == 0,
                    Lit::I64(n) => n == 0,
                    _ => false,
                };
                if zero {
                    on_zero(self);
                    return Ok(());
                }
                self.prim_on(p, ty, &[x.clone(), y.clone()])
            }
            Atom::Var(_) => {
                // Only the low bits of an `i8` or `i16` are its value.
                if narrow_bits(ty).is_some() {
                    self.int(y, ty, Sign::Unsigned);
                } else {
                    self.atom(y);
                }
                self.emit(if wide { I::I64Eqz } else { I::I32Eqz });
                let result = if wide { ValType::I64 } else { ValType::I32 };
                self.open(I::If(BlockType::Result(result)));
                on_zero(self);
                self.emit(I::Else);
                self.prim_on(p, ty, &[x.clone(), y.clone()])?;
                self.close();
                Ok(())
            }
        }
    }

    /// `p` on `args`, whose first has type `ty`.
    fn prim_on(&mut self, p: &Prim, ty: &Ty, args: &[Atom]) -> Result<(), String> {
        let first = &args[0];
        let ty = ty.clone();
        if let Some(bits) = narrow_bits(&ty) {
            return self.narrow_prim(p, bits, &ty, args);
        }
        if let Prim::Convert(to, s) = p
            && let Some(bits) = narrow_bits(to)
        {
            // Only the low bits matter, so from an integer this keeps them.
            self.atom(first);
            match ty {
                Ty::I32 => {}
                Ty::I64 => self.emit(I::I32WrapI64),
                Ty::F32 | Ty::F64 => {
                    self.emit(match (&ty, s) {
                        (Ty::F32, Sign::Signed) => I::I32TruncF32S,
                        (Ty::F32, Sign::Unsigned) => I::I32TruncF32U,
                        (_, Sign::Signed) => I::I32TruncF64S,
                        (_, Sign::Unsigned) => I::I32TruncF64U,
                    });
                    self.check_fits(bits, *s);
                }
                _ => return Err(format!("{p:?} isn't defined on {ty}")),
            }
            return Ok(());
        }
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

    /// A prim on `i8`s or `i16`s, done on `i32`s.
    fn narrow_prim(&mut self, p: &Prim, bits: u32, ty: &Ty, args: &[Atom]) -> Result<(), String> {
        use Sign::{Signed as S, Unsigned as U};
        let undefined = || Err(format!("{p:?} isn't defined on {ty}"));
        // Both operands extended as `sign` says, then `i`.
        let both = |cx: &mut Self, sign: Sign, i: I<'static>| {
            cx.int(&args[0], ty, sign);
            cx.int(&args[1], ty, sign);
            cx.emit(i);
        };
        let pick = |s: &Sign, signed: I<'static>, unsigned: I<'static>| match s {
            S => signed,
            U => unsigned,
        };
        match p {
            // The low bits of these only depend on the low bits of the
            // operands.
            Prim::Add | Prim::Sub | Prim::Mul | Prim::And | Prim::Or | Prim::Xor => {
                self.atoms(args);
                self.emit(match p {
                    Prim::Add => I::I32Add,
                    Prim::Sub => I::I32Sub,
                    Prim::Mul => I::I32Mul,
                    Prim::And => I::I32And,
                    Prim::Or => I::I32Or,
                    _ => I::I32Xor,
                });
            }
            Prim::Neg => {
                self.emit(I::I32Const(0));
                self.atom(&args[0]);
                self.emit(I::I32Sub);
            }
            // The count is taken modulo the width, as for `i32` and `i64`.
            Prim::Shl | Prim::Shr(_) => {
                match p {
                    Prim::Shr(s) => self.int(&args[0], ty, *s),
                    _ => self.atom(&args[0]),
                }
                self.atom(&args[1]);
                self.emit(I::I32Const(bits as i32 - 1));
                self.emit(I::I32And);
                self.emit(match p {
                    Prim::Shr(s) => pick(s, I::I32ShrS, I::I32ShrU),
                    _ => I::I32Shl,
                });
            }
            Prim::Div(s) => {
                both(self, *s, pick(s, I::I32DivS, I::I32DivU));
                // The smallest value divided by -1 overflows, which traps as
                // it does for `i32`.
                if *s == S {
                    self.check_fits(bits, S);
                }
            }
            Prim::Rem(s) => both(self, *s, pick(s, I::I32RemS, I::I32RemU)),
            Prim::Eq => both(self, U, I::I32Eq),
            Prim::Ne => both(self, U, I::I32Ne),
            Prim::Lt(s) => both(self, *s, pick(s, I::I32LtS, I::I32LtU)),
            Prim::Le(s) => both(self, *s, pick(s, I::I32LeS, I::I32LeU)),
            Prim::Gt(s) => both(self, *s, pick(s, I::I32GtS, I::I32GtU)),
            Prim::Ge(s) => both(self, *s, pick(s, I::I32GeS, I::I32GeU)),
            Prim::Convert(to, s) => {
                let to_bits = narrow_bits(to);
                match to {
                    // Narrower or the same: the low bits are already right.
                    _ if to_bits.is_some_and(|b| b <= bits) => self.atom(&args[0]),
                    Ty::I8 | Ty::I16 | Ty::I32 => self.int(&args[0], ty, *s),
                    Ty::I64 => {
                        self.int(&args[0], ty, *s);
                        self.emit(pick(s, I::I64ExtendI32S, I::I64ExtendI32U));
                    }
                    Ty::F32 => {
                        self.int(&args[0], ty, *s);
                        self.emit(pick(s, I::F32ConvertI32S, I::F32ConvertI32U));
                    }
                    Ty::F64 => {
                        self.int(&args[0], ty, *s);
                        self.emit(pick(s, I::F64ConvertI32S, I::F64ConvertI32U));
                    }
                    _ => return undefined(),
                }
            }
            _ => return undefined(),
        }
        Ok(())
    }
}
/// How many bits an `i8` or `i16` has; `None` for other types.
fn narrow_bits(ty: &Ty) -> Option<u32> {
    match ty {
        Ty::I8 => Some(8),
        Ty::I16 => Some(16),
        _ => None,
    }
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

/// The default value of a type, for a field that's set before it's read.
fn zero(ty: ValType) -> I<'static> {
    match ty {
        ValType::I32 => I::I32Const(0),
        ValType::I64 => I::I64Const(0),
        ValType::F32 => I::F32Const(Ieee32::new(0)),
        ValType::F64 => I::F64Const(Ieee64::new(0)),
        ValType::V128 => I::V128Const(0),
        ValType::Ref(r) => I::RefNull(r.heap_type),
    }
}

/// Whether `ty` is a scalar, which a box holds in memory.
fn is_scalar_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64)
}
