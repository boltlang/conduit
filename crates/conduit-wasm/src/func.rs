//! How a function body becomes Wasm code. See "Lowering" in `docs/ir.md`.
//!
//! Every variable is a local, and every block ends in a `return`, `br`,
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
//!
//! An `i8` or `i16` is an `i32` local whose low 8 or 16 bits are the value,
//! as in LLVM's Wasm backend. The high bits can be anything, so ops that
//! read them first sign- or zero-extend: `div`, `rem`, `shr`, comparisons,
//! conversions to wider types, and `case`. Ops whose low bits only depend
//! on the low bits of their operands (`add`, `mul`, `and`, `shl`, ..)
//! don't, and an array of them stores only the low bits.

use std::borrow::Cow;

use conduit_ir::{
    Atom, Block, CtorId, EffectId, Func, HandlerId, JoinDef, Label, Lit, Op, Prim, Sign, Tag, Tail,
    Ty, Var,
};
use wasm_encoder::{
    BlockType, Catch, HeapType, Ieee32, Ieee64, Instruction as I, RefType, ValType,
};

use crate::types::nullable;
use crate::{ABORT, Cx, HANDLERS, SITE};

/// A lowered function: the locals after its parameters, and its code.
pub(crate) struct Body {
    pub locals: Vec<ValType>,
    pub code: Vec<I<'static>>,
    /// The names of locals that hold named variables, by local index.
    pub local_names: Vec<(u32, Box<str>)>,
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
    fcx.block(&f.body)?;
    fcx.code.push(I::End);
    let mut local_names: Vec<(u32, Box<str>)> = f
        .debug
        .vars
        .iter()
        .filter_map(|(v, name)| Some((*fcx.locals.get(v.0 as usize)?, name.clone())))
        .collect();
    local_names.sort();
    Ok(Body {
        locals: fcx.extra,
        code: fcx.code,
        local_names,
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
        local_names: Vec::new(),
    })
}

/// The wrapper of clause `i` of `h`, which `perform` calls with the frame
/// and the operation's parameters: it reads the environment out of the
/// frame and tail-calls the clause, as a plain function. For an aborting
/// clause, it stores what the clause gives in the frame, and throws.
pub(crate) fn lower_wrapper(cx: &mut Cx<'_>, h: HandlerId, i: u32) -> Result<Body, String> {
    let decl = &cx.m.handlers[&h];
    let (frame, first) = cx.types.handler_frame(h)?;
    let Some(op) =
        cx.m.effects
            .get(&decl.effect)
            .and_then(|e| e.ops.get(i as usize))
    else {
        return Err(format!("{} has no operation {i}", decl.effect));
    };
    let params = op.params.len() as u32;
    let local = params + 1;
    let mut code = vec![
        I::LocalGet(0),
        I::RefCastNullable(HeapType::Concrete(frame)),
        I::LocalSet(local),
    ];
    for j in 0..decl.env.len() as u32 {
        code.push(I::LocalGet(local));
        code.push(I::StructGet {
            struct_type_index: frame,
            field_index: first + j,
        });
    }
    for p in 0..params {
        code.push(I::LocalGet(1 + p));
    }
    let clause = decl.ops[i as usize];
    let idx = cx.func_index(clause)?;
    let mut locals = vec![nullable(frame)];
    if cx.aborts.contains(&clause) {
        code.push(I::Call(idx));
        let results = cx.types.vals(&cx.m.funcs[&clause].results)?;
        let first_result = first + decl.env.len() as u32;
        let n = results.len() as u32;
        locals.extend(results);
        for j in (0..n).rev() {
            code.push(I::LocalSet(local + 1 + j));
        }
        for j in 0..n {
            code.push(I::LocalGet(local));
            code.push(I::LocalGet(local + 1 + j));
            code.push(I::StructSet {
                struct_type_index: frame,
                field_index: first_result + j,
            });
        }
        code.push(I::LocalGet(0));
        code.push(I::Throw(ABORT));
    } else {
        code.push(I::ReturnCall(idx));
    }
    code.push(I::End);
    Ok(Body {
        locals,
        code,
        local_names: Vec::new(),
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

    fn is_cont(&self, a: &Atom) -> bool {
        matches!(self.atom_ty(a), Ty::Cont(_))
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
            Some(d) if !self.cx.types.tagged(d)? => Some(arms.first().map(|a| &a.body).or(default)),
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
                let (entry, captures) = self.cx.entry(*f, args.len())?;
                self.emit(I::RefFunc(entry));
                self.atoms(args);
                self.emit(I::StructNew(captures));
            }
            Op::Apply => {
                let Some((c, rest)) = args.split_first() else {
                    return Err("apply needs a closure".into());
                };
                self.apply(c, rest, tail)?;
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
            Op::Const(id) => {
                let global = self.cx.const_global(*id)?;
                self.emit(I::GlobalGet(global));
            }
            // Every type has a default: 0, or null.
            Op::Undefined => {
                let ty = self.cx.types.val(self.var_ty(binds[0]))?;
                self.emit(zero(ty));
            }
            Op::Get(c, i) => {
                let (idx, first) = self.object(*c, &args[0])?;
                // An `i8` or `i16` field is packed, as array elements are.
                // Whether the field is one doesn't depend on mutabilities.
                let packed = self
                    .cx
                    .m
                    .data
                    .get(&c.data)
                    .and_then(|d| d.ctors.get(c.index as usize))
                    .and_then(|ctor| ctor.fields.get(*i as usize))
                    .is_some_and(|ty| narrow_bits(ty).is_some());
                let at = (idx, first + i);
                self.emit(if packed {
                    I::StructGetS {
                        struct_type_index: at.0,
                        field_index: at.1,
                    }
                } else {
                    I::StructGet {
                        struct_type_index: at.0,
                        field_index: at.1,
                    }
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
                let ty = self.atom_ty(&args[0]);
                let array = self.array_of(&ty)?;
                self.atoms(args);
                let packed = matches!(&ty, Ty::Array(elem, _) if narrow_bits(elem).is_some());
                self.emit(match op {
                    // Either extension gives the right low bits.
                    Op::ArrayGet if packed => I::ArrayGetS(array),
                    Op::ArrayGet => I::ArrayGet(array),
                    _ => I::ArraySet(array),
                });
            }
            Op::ArrayLen => {
                self.atoms(args);
                self.emit(I::ArrayLen);
            }
            Op::ArrayAlloc => {
                // Every element type has a default, since references are
                // nullable: unset elements are null or 0.
                let array = self.array_of(self.var_ty(binds[0]))?;
                self.atom(&args[0]);
                self.emit(I::ArrayNewDefault(array));
            }
            Op::ArrayCopy => {
                // Wasm takes the same arguments in the same order, and
                // copies as through a temporary too.
                let dst = self.array_of(&self.atom_ty(&args[0]))?;
                let src = self.array_of(&self.atom_ty(&args[2]))?;
                self.atoms(args);
                self.emit(I::ArrayCopy {
                    array_type_index_dst: dst,
                    array_type_index_src: src,
                });
            }
            Op::ArrayClone => {
                // A new array as long as the original, then all of the
                // original's elements copied into it. Mutability doesn't
                // exist at run time, so both have the same Wasm type.
                let array = self.array_of(self.var_ty(binds[0]))?;
                let val = self.cx.types.val(self.var_ty(binds[0]))?;
                let copy = self.new_local(val);
                self.atom(&args[0]);
                self.emit(I::ArrayLen);
                self.emit(I::ArrayNewDefault(array));
                self.emit(I::LocalTee(copy));
                self.emit(I::I32Const(0));
                self.atom(&args[0]);
                self.emit(I::I32Const(0));
                self.atom(&args[0]);
                self.emit(I::ArrayLen);
                self.emit(I::ArrayCopy {
                    array_type_index_dst: array,
                    array_type_index_src: array,
                });
                self.emit(I::LocalGet(copy));
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
            // Mutability doesn't exist at run time.
            Op::ArrayFreeze => self.atoms(args),
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
                self.call_closure(k, rest, tail)?;
                return Ok(self.bind(binds, tail));
            }
            // In a tail-resumptive clause, without its continuation: the
            // `perform` it handles performs this instead, so it runs under
            // that `perform`'s handlers.
            Op::Discontinue(e, i) if args.first().is_none_or(|a| !self.is_cont(a)) => {
                self.emit(I::GlobalGet(SITE));
                self.emit(I::GlobalSet(HANDLERS));
                self.perform(*e, *i, args)?;
            }
            Op::Discontinue(e, i) => {
                self.discontinue(*e, *i, args, tail)?;
                return Ok(self.bind(binds, tail));
            }
        }
        Ok(self.bind(binds, false))
    }

    /// Calls closure `c` with `args`.
    fn apply(&mut self, c: &Atom, args: &[Atom], tail: bool) -> Result<(), String> {
        if !matches!(self.atom_ty(c), Ty::Fn(..)) {
            return Err("apply of a value that isn't a closure".into());
        }
        self.call_closure(c, args, tail)
    }

    /// Calls `c`, a closure or a continuation, with `args`.
    fn call_closure(&mut self, c: &Atom, args: &[Atom], tail: bool) -> Result<(), String> {
        let (Ty::Fn(sig, _) | Ty::Cont(sig)) = self.atom_ty(c) else {
            return Err("a call of a value that isn't a closure".into());
        };
        let sig = self.cx.types.sig(&sig)?;
        let base = self.cx.types.closure(&sig)?;
        let entry = self.cx.types.entry(&sig)?;
        self.atom(c);
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
        Ok(())
    }

    /// `handle h(env.., body)`: pushes a frame of `h` on the handler stack,
    /// runs the body, pops the frame and calls the return clause with the
    /// body's results. If `h` has an aborting clause, the body runs in a
    /// `try_table` that catches the exception holding this frame, and the
    /// results are then the frame's. Returns whether it was a tail call.
    fn handle(&mut self, h: HandlerId, args: &[Atom], tail: bool) -> Result<bool, String> {
        if let Some(switch) = self.cx.switch(h) {
            // A frame of its own, linked in by `start`, which runs the body.
            let start = switch.start;
            let number = self.cx.effect_number(self.cx.m.handlers[&h].effect)?;
            let frame = self.cx.types.switch_frame()?;
            let base = self.cx.types.frame()?;
            self.emit(I::I32Const(number));
            self.emit(I::RefNull(HeapType::Concrete(base)));
            self.emit(I::StructNew(frame));
            self.atoms(args);
            self.emit(if tail {
                I::ReturnCall(start)
            } else {
                I::Call(start)
            });
            return Ok(tail);
        }
        if self.cx.types.aborting.contains(&h) {
            self.handle_aborting(h, args)?;
            return Ok(false);
        }
        let first_wrapper = self.cx.wrapper(h, 0)?;
        let decl = &self.cx.m.handlers[&h];
        let (env, body) = args.split_at(args.len() - 1);
        let (frame, _) = self.cx.types.handler_frame(h)?;
        let base = nullable(self.cx.types.frame()?);
        let number = self.cx.effect_number(decl.effect)?;
        let ret = self.cx.func_index(decl.ret)?;
        let Ty::Fn(sig, _) = self.atom_ty(&body[0]) else {
            return Err(format!("the body of handle {h} isn't a closure"));
        };
        let results: Vec<u32> = self
            .cx
            .types
            .vals(&sig.results)?
            .into_iter()
            .map(|ty| self.new_local(ty))
            .collect();

        let saved = self.new_local(base);
        self.emit(I::GlobalGet(HANDLERS));
        self.emit(I::LocalSet(saved));
        self.emit(I::I32Const(number));
        self.emit(I::LocalGet(saved));
        for i in 0..decl.ops.len() as u32 {
            self.emit(I::RefFunc(first_wrapper + i));
        }
        self.atoms(env);
        self.emit(I::StructNew(frame));
        self.emit(I::GlobalSet(HANDLERS));
        self.apply(&body[0], &[], false)?;
        for &r in results.iter().rev() {
            self.emit(I::LocalSet(r));
        }
        self.emit(I::LocalGet(saved));
        self.emit(I::GlobalSet(HANDLERS));
        self.atoms(env);
        for &r in &results {
            self.emit(I::LocalGet(r));
        }
        self.emit(if tail {
            I::ReturnCall(ret)
        } else {
            I::Call(ret)
        });
        Ok(tail)
    }

    /// `handle` of a handler with an aborting clause.
    fn handle_aborting(&mut self, h: HandlerId, args: &[Atom]) -> Result<(), String> {
        let first_wrapper = self.cx.wrapper(h, 0)?;
        let decl = &self.cx.m.handlers[&h];
        let (env, body) = args.split_at(args.len() - 1);
        let (frame, first) = self.cx.types.handler_frame(h)?;
        let base = nullable(self.cx.types.frame()?);
        let number = self.cx.effect_number(decl.effect)?;
        let ret = self.cx.func_index(decl.ret)?;
        let Ty::Fn(sig, _) = self.atom_ty(&body[0]) else {
            return Err(format!("the body of handle {h} isn't a closure"));
        };
        let Some(ret_func) = self.cx.m.funcs.get(&decl.ret) else {
            return Err(format!("unknown return clause {}", decl.ret));
        };
        let body_tys = self.cx.types.vals(&sig.results)?;
        let out_tys = self.cx.types.vals(&ret_func.results)?;
        let first_result = first + env.len() as u32;
        let results: Vec<u32> = body_tys.into_iter().map(|t| self.new_local(t)).collect();
        let outs: Vec<u32> = out_tys.iter().map(|&t| self.new_local(t)).collect();
        let saved = self.new_local(base);
        let me = self.new_local(nullable(frame));
        let thrown = self.new_local(base);
        let site = self.cx.discontinues.then(|| self.new_local(base));

        if let Some(site) = site {
            self.emit(I::GlobalGet(SITE));
            self.emit(I::LocalSet(site));
        }
        self.emit(I::GlobalGet(HANDLERS));
        self.emit(I::LocalSet(saved));
        self.emit(I::I32Const(number));
        self.emit(I::LocalGet(saved));
        for i in 0..decl.ops.len() as u32 {
            self.emit(I::RefFunc(first_wrapper + i));
        }
        self.atoms(env);
        for &ty in &out_tys {
            self.emit(zero(ty));
        }
        self.emit(I::StructNew(frame));
        self.emit(I::LocalTee(me));
        self.emit(I::GlobalSet(HANDLERS));

        self.open(I::Block(BlockType::Empty));
        self.open(I::Block(BlockType::Result(base)));
        // A catch's label is counted from outside the `try_table`.
        let catches = vec![Catch::One {
            tag: ABORT,
            label: 0,
        }];
        self.open(I::TryTable(BlockType::Empty, Cow::Owned(catches)));
        self.apply(&body[0], &[], false)?;
        for &r in results.iter().rev() {
            self.emit(I::LocalSet(r));
        }
        self.close();
        // The body returned: run the return clause outside the handler.
        self.emit(I::LocalGet(saved));
        self.emit(I::GlobalSet(HANDLERS));
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
        self.emit(I::LocalGet(me));
        self.emit(I::RefEq);
        self.emit(I::I32Eqz);
        self.open(I::If(BlockType::Empty));
        self.emit(I::LocalGet(thrown));
        self.emit(I::Throw(ABORT));
        self.close();
        // The handler stack is already `saved`: the `perform` set it to the
        // frames below this one before calling the clause. The `perform`s
        // the exception left didn't put back the stack of the clause they
        // ran, so this does.
        if let Some(site) = site {
            self.emit(I::LocalGet(site));
            self.emit(I::GlobalSet(SITE));
        }
        for (j, &o) in outs.iter().enumerate() {
            self.emit(I::LocalGet(me));
            self.emit(I::StructGet {
                struct_type_index: frame,
                field_index: first_result + j as u32,
            });
            self.emit(I::LocalSet(o));
        }
        self.close();
        for &o in &outs {
            self.emit(I::LocalGet(o));
        }
        Ok(())
    }

    /// `perform e.i(args)`: finds the nearest frame of `e`, trapping if
    /// there's none, and calls its clause with the handler stack below the
    /// frame, so that a `perform` in the clause goes to an outer handler.
    fn perform(&mut self, e: EffectId, i: u32, args: &[Atom]) -> Result<(), String> {
        let Some(tag) = self.cx.tag(e, i) else {
            return self.perform_call(e, i, args);
        };
        // The nearest frame may be of a handler that switches stacks, or
        // not: which one is only known when it runs. The results go
        // through locals, as both ways give them.
        let m = self.cx.m;
        let op = &m.effects[&e].ops[i as usize];
        let tys = self.cx.types.vals(&op.results)?;
        let results: Vec<u32> = tys.iter().map(|&t| self.new_local(t)).collect();
        let base = self.cx.types.frame()?;
        let switch_frame = self.cx.types.switch_frame()?;
        let saved = self.new_local(nullable(base));
        self.emit(I::GlobalGet(HANDLERS));
        self.emit(I::LocalSet(saved));
        let found = self.find_frame(e)?;
        self.emit(I::LocalGet(found));
        self.emit(I::RefTestNonNull(HeapType::Concrete(switch_frame)));
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
                // at it, which performs what it gives instead.
                let (_, thunk_sig) = self.cx.types.thunk(e, i)?;
                let thunk = self.cx.types.closure(&thunk_sig)?;
                let entry = self.cx.types.entry(&thunk_sig)?;
                let t = self.new_local(nullable(thunk));
                self.open(I::Block(BlockType::Empty));
                self.open(I::Block(BlockType::Result(ValType::Ref(RefType::ANYREF))));
                let catches = vec![Catch::One {
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
                self.emit(I::RefCastNullable(HeapType::Concrete(thunk)));
                self.emit(I::LocalSet(t));
                self.emit(I::LocalGet(saved));
                self.emit(I::GlobalSet(HANDLERS));
                self.emit(I::LocalGet(t));
                self.emit(I::LocalGet(t));
                self.emit(I::StructGet {
                    struct_type_index: thunk,
                    field_index: 0,
                });
                self.emit(I::CallRef(entry));
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
        self.perform_call(e, i, args)?;
        for &r in results.iter().rev() {
            self.emit(I::LocalSet(r));
        }
        self.close();
        for &r in &results {
            self.emit(I::LocalGet(r));
        }
        Ok(())
    }

    /// The local holding the nearest frame of `e` on the handler stack,
    /// trapping if there's none.
    fn find_frame(&mut self, e: EffectId) -> Result<u32, String> {
        let base = self.cx.types.frame()?;
        let number = self.cx.effect_number(e)?;
        let found = self.new_local(nullable(base));
        self.emit(I::GlobalGet(HANDLERS));
        self.emit(I::LocalSet(found));
        self.open(I::Block(BlockType::Empty));
        self.open(I::Loop(BlockType::Empty));
        // Reading the effect of a null frame traps.
        self.emit(I::LocalGet(found));
        self.emit(I::StructGet {
            struct_type_index: base,
            field_index: 0,
        });
        self.emit(I::I32Const(number));
        self.emit(I::I32Eq);
        self.emit(I::BrIf(1));
        self.emit(I::LocalGet(found));
        self.emit(I::StructGet {
            struct_type_index: base,
            field_index: 1,
        });
        self.emit(I::LocalSet(found));
        self.emit(I::Br(0));
        self.close();
        self.close();
        Ok(found)
    }

    /// `discontinue F.j(k, args)` of a continuation: throws a closure that
    /// performs `F.j(args)` into it.
    fn discontinue(
        &mut self,
        e: EffectId,
        i: u32,
        args: &[Atom],
        tail: bool,
    ) -> Result<(), String> {
        let Some((k, rest)) = args.split_first() else {
            return Err("discontinue without a continuation".into());
        };
        let Ty::Cont(sig) = self.atom_ty(k) else {
            return Err("discontinue of something that isn't a continuation".into());
        };
        let sig = self.cx.types.sig(&sig)?;
        let base = self.cx.types.cont_base(&sig)?;
        let disc = self.cx.types.discontinue(&sig)?;
        let (thunk, _) = self.cx.types.thunk(e, i)?;
        let (_, entry) = self
            .cx
            .thunks
            .get(&(e, i))
            .copied()
            .ok_or_else(|| format!("no thunk for {e}.{i}"))?;
        self.atom(k);
        self.emit(I::RefFunc(entry));
        self.atoms(rest);
        self.emit(I::StructNew(thunk));
        self.atom(k);
        self.emit(I::StructGet {
            struct_type_index: base,
            field_index: 1,
        });
        self.emit(if tail {
            I::ReturnCallRef(disc)
        } else {
            I::CallRef(disc)
        });
        Ok(())
    }

    /// `perform e.i(args)` of a handler that doesn't switch stacks.
    fn perform_call(&mut self, e: EffectId, i: u32, args: &[Atom]) -> Result<(), String> {
        let base = self.cx.types.frame()?;
        let frame = self.cx.types.effect_frame(e)?;
        let clause = self.cx.types.clause(e, i)?;
        let number = self.cx.effect_number(e)?;
        let saved = self.new_local(nullable(base));
        let found = self.new_local(nullable(base));
        let this = self.new_local(nullable(frame));

        self.emit(I::GlobalGet(HANDLERS));
        self.emit(I::LocalTee(saved));
        self.emit(I::LocalSet(found));
        self.open(I::Block(BlockType::Empty));
        self.open(I::Loop(BlockType::Empty));
        // Reading the effect of a null frame traps.
        self.emit(I::LocalGet(found));
        self.emit(I::StructGet {
            struct_type_index: base,
            field_index: 0,
        });
        self.emit(I::I32Const(number));
        self.emit(I::I32Eq);
        self.emit(I::BrIf(1));
        self.emit(I::LocalGet(found));
        self.emit(I::StructGet {
            struct_type_index: base,
            field_index: 1,
        });
        self.emit(I::LocalSet(found));
        self.emit(I::Br(0));
        self.close();
        self.close();

        self.emit(I::LocalGet(found));
        self.emit(I::StructGet {
            struct_type_index: base,
            field_index: 1,
        });
        self.emit(I::GlobalSet(HANDLERS));
        self.emit(I::LocalGet(found));
        self.emit(I::RefCastNullable(HeapType::Concrete(frame)));
        self.emit(I::LocalTee(this));
        // The clause can discontinue under this `perform`'s handlers.
        let site = self.cx.discontinues.then(|| {
            let old = self.new_local(nullable(base));
            self.emit(I::GlobalGet(SITE));
            self.emit(I::LocalSet(old));
            self.emit(I::LocalGet(saved));
            self.emit(I::GlobalSet(SITE));
            old
        });
        self.atoms(args);
        self.emit(I::LocalGet(this));
        self.emit(I::StructGet {
            struct_type_index: frame,
            field_index: 2 + i,
        });
        self.emit(I::CallRef(clause));
        self.emit(I::LocalGet(saved));
        self.emit(I::GlobalSet(HANDLERS));
        if let Some(old) = site {
            self.emit(I::LocalGet(old));
            self.emit(I::GlobalSet(SITE));
        }
        Ok(())
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
