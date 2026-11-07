//! Handlers that switch stacks, with the stack switching proposal: those
//! with a clause that's neither tail-resumptive nor aborting, when
//! [`Options::stack_switching`](crate::Options) asks for it. Their bodies
//! run as Wasm continuations, and a `perform` to one suspends:
//!
//! - A handler's frame has -1 in every clause slot, which tells `perform`
//!   to suspend with the operation's tag instead of calling a clause.
//! - `handle` calls the handler's `start`, which makes the body a Wasm
//!   continuation of a trampoline that applies the body's closure, and
//!   resumes it with a label per operation.
//! - A Wasm continuation is a reference, which memory can't hold, so a
//!   captured one goes in a table of its type, one per operation, and the
//!   Conduit continuation holds its slot.
//! - A Conduit continuation is a closure: the header, its wrapper's table
//!   slot at 4, then, laid out from 8, the slot of the function that
//!   discontinues it, the frame, the handler's environment, the slot of
//!   the Wasm continuation, and when counting, whether it was used. Its
//!   wrapper resumes it under the handler again through `drive`, so the
//!   handler is deep, and a second resume traps.
//! - When counting, a Conduit continuation is counted, and holds a
//!   reference to the environment. Resuming or discontinuing it takes it:
//!   it's marked used, what it holds is dupped for `drive`, its slot goes
//!   on its table's free list, which `drive` takes slots from before it
//!   grows the table, and it's dropped. Freed without being used, it gives
//!   back the handler's frame, the frame's segment and its slot; what its
//!   own stack holds is lost. Otherwise `drive` gives the frame back when
//!   the body returns or throws.
//! - `discontinue F.j(k, args)` makes a closure that performs `F.j(args)`
//!   and throws its address into `k` with `resume_throw`, which the
//!   `perform` catches and calls, under its own handlers.
//! - With a shadow stack, each continuation has segments of its own (see
//!   [`ShadowStack`]), the first of which `start` takes and keeps in the
//!   frame. `start`, `drive` and `throw` switch to it, and
//!   back to the outer stack's when the body suspends, returns or throws,
//!   giving it back in the last two cases. Without counts, a continuation
//!   that's never resumed again keeps its segment and its table slot.

use conduit_ir::HandlerId;
use wasm_encoder::{BlockType, Catch, Handle, HeapType, Instruction as I, RefType, ValType};

use crate::counts::{DropKey, LIVE, key_of};
use crate::func::{Body, load, store};
use crate::{Cx, FnSig, HANDLERS, HEADER, ShadowStack, Storage, layout, shadow, val};

/// How a handler's `start`, `drive` or `throw` function runs its body.
#[derive(Clone, Copy)]
pub(crate) enum Start {
    /// `start`: from the beginning, given the body's closure.
    Body,
    /// `drive`: resuming operation `i`'s continuation with its results.
    Resume(u32),
    /// `throw`: throwing a closure into operation `i`'s continuation.
    Throw(u32),
}

/// A nullable reference to the type at `idx`.
pub(crate) fn nullable(idx: u32) -> ValType {
    ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(idx),
    })
}

impl Cx<'_> {
    /// The results of the body of a `handle` of `h`: what its return
    /// clause takes after the environment.
    pub fn body_results(&self, h: HandlerId) -> Result<Vec<ValType>, String> {
        let decl = &self.m.handlers[&h];
        let ret = self
            .m
            .funcs
            .get(&decl.ret)
            .ok_or_else(|| format!("unknown return clause {}", decl.ret))?;
        let sig = ret.sig();
        Ok(sig.params[decl.env.len()..].iter().map(val).collect())
    }

    /// What a `handle` of `h` gives.
    pub fn handle_results(&self, h: HandlerId) -> Result<Vec<ValType>, String> {
        let decl = &self.m.handlers[&h];
        let ret = self
            .m
            .funcs
            .get(&decl.ret)
            .ok_or_else(|| format!("unknown return clause {}", decl.ret))?;
        Ok(ret.results.iter().map(val).collect())
    }

    /// The Wasm continuation captured by operation `i` under `h`: resumed
    /// with the operation's results, it gives the body's.
    pub fn op_cont(&mut self, h: HandlerId, i: u32) -> Result<u32, String> {
        let decl = &self.m.handlers[&h];
        let op = crate::op_at(self.m, decl, i);
        let sig = FnSig {
            params: op.results.iter().map(val).collect(),
            results: self.body_results(h)?,
        };
        Ok(self.cont(sig))
    }

    /// The continuation of `h`'s trampoline, which runs a body's closure.
    pub fn start_cont(&mut self, h: HandlerId) -> Result<u32, String> {
        let sig = FnSig {
            params: vec![ValType::I32],
            results: self.body_results(h)?,
        };
        Ok(self.cont(sig))
    }

    /// The layout of a Conduit continuation of `h`'s clause `i`, from 8:
    /// the slot of its discontinue function, the frame, the environment,
    /// the slot of the Wasm continuation, and when counting, whether it
    /// was used.
    pub fn kont_layout(&self, h: HandlerId) -> (Vec<(u32, Storage)>, u32) {
        let decl = &self.m.handlers[&h];
        let mut tys = vec![conduit_ir::Ty::I32, conduit_ir::Ty::I32];
        tys.extend(decl.env.iter().cloned());
        tys.push(conduit_ir::Ty::I32);
        if self.rt.is_some() {
            tys.push(conduit_ir::Ty::I32);
        }
        layout(&tys)
    }

    /// Puts the Wasm continuation in local `k` in a slot of table `t`,
    /// into local `slot`: a free one, with the free list's node in local
    /// `node` going back to the spares, or else a new one.
    fn take_slot(&self, t: u32, k: u32, slot: u32, node: u32) -> Vec<I<'static>> {
        let grow = [
            I::LocalGet(k),
            I::I32Const(1),
            I::TableGrow(t),
            I::LocalTee(slot),
            I::I32Const(-1),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::Unreachable,
            I::End,
        ];
        let Some(lists) = &self.slot_lists else {
            return grow.to_vec();
        };
        let (head, spare) = (lists.heads[t as usize - 1], lists.spare);
        let mut code = vec![
            I::GlobalGet(head),
            I::LocalTee(node),
            I::If(BlockType::Empty),
            I::LocalGet(node),
            load(Storage::I32, 0),
            I::GlobalSet(head),
            I::LocalGet(node),
            load(Storage::I32, 4),
            I::LocalSet(slot),
            I::LocalGet(node),
            I::GlobalGet(spare),
            store(Storage::I32, 0),
            I::LocalGet(node),
            I::GlobalSet(spare),
            I::LocalGet(slot),
            I::LocalGet(k),
            I::TableSet(t),
            I::Else,
        ];
        code.extend(grow);
        code.push(I::End);
        code
    }

    /// Empties the slot of table `t`, of continuations of type `cont`,
    /// that `slot` gives, and puts it on its free list, with a spare node
    /// in local `node`, or else a new one, which isn't an object.
    fn free_slot(&self, t: u32, cont: u32, slot: &[I<'static>], node: u32) -> Vec<I<'static>> {
        let lists = self.slot_lists.as_ref().expect("free lists of slots");
        let (head, spare) = (lists.heads[t as usize - 1], lists.spare);
        let mut code = slot.to_vec();
        code.extend([
            I::RefNull(HeapType::Concrete(cont)),
            I::TableSet(t),
            I::GlobalGet(spare),
            I::LocalTee(node),
            I::If(BlockType::Empty),
            I::LocalGet(node),
            load(Storage::I32, 0),
            I::GlobalSet(spare),
            I::Else,
            I::I32Const(8),
            I::Call(self.alloc),
            I::LocalSet(node),
            I::GlobalGet(LIVE),
            I::I32Const(1),
            I::I32Sub,
            I::GlobalSet(LIVE),
            I::End,
            I::LocalGet(node),
        ]);
        code.extend(slot.iter().cloned());
        code.extend([
            store(Storage::I32, 4),
            I::LocalGet(node),
            I::GlobalGet(head),
            store(Storage::I32, 0),
            I::LocalGet(node),
            I::GlobalSet(head),
        ]);
        code
    }

    /// The parameters of `h`'s `start`, `drive` or `throw` function, and
    /// its results.
    pub fn drive_sig(&mut self, h: HandlerId, start: Start) -> Result<FnSig, String> {
        let decl = &self.m.handlers[&h];
        let mut params = vec![ValType::I32];
        params.extend(decl.env.iter().map(val));
        match start {
            Start::Body => params.push(ValType::I32),
            Start::Resume(i) => {
                params.push(nullable(self.op_cont(h, i)?));
                let op = crate::op_at(self.m, decl, i);
                params.extend(op.results.iter().map(val));
            }
            Start::Throw(i) => {
                params.push(nullable(self.op_cont(h, i)?));
                params.push(ValType::I32);
            }
        }
        Ok(FnSig {
            params,
            results: self.handle_results(h)?,
        })
    }

    /// The type of the function that discontinues a continuation that
    /// gives `results`: `(k, thunk) -> results`.
    pub fn disc_sig(&mut self, results: Vec<ValType>) -> u32 {
        self.sig(FnSig {
            params: vec![ValType::I32, ValType::I32],
            results,
        })
    }
}

/// The trampoline of `h`, `(closure) -> body results`, which the body
/// runs in as a Wasm continuation: it applies the closure.
pub(crate) fn lower_tramp(cx: &mut Cx<'_>, h: HandlerId) -> Result<Body, String> {
    let results = cx.body_results(h)?;
    let entry = cx.sig(FnSig {
        params: vec![ValType::I32],
        results,
    });
    let code = vec![
        I::LocalGet(0),
        I::LocalGet(0),
        load(Storage::I32, 4),
        I::ReturnCallIndirect {
            type_index: entry,
            table_index: 0,
        },
        I::End,
    ];
    Ok(Body {
        locals: Vec::new(),
        code,
    })
}

/// The `start`, `drive` or `throw` function of `h`.
pub(crate) fn lower_drive(cx: &mut Cx<'_>, h: HandlerId, start: Start) -> Result<Body, String> {
    let m = cx.m;
    let decl = &m.handlers[&h];
    let n = decl.len() as u32;
    let sig = cx.drive_sig(h, start)?;
    let env = decl.env.len() as u32;
    let sw = cx.switch(h)?.clone();
    let mut locals: Vec<ValType> = Vec::new();
    let first_local = sig.params.len() as u32;
    let new_local = |locals: &mut Vec<ValType>, ty: ValType| {
        locals.push(ty);
        first_local + locals.len() as u32 - 1
    };
    // With a shadow stack, the continuation runs on a segment of its
    // own, and the resume is in a `try_table`, one more label in.
    let shadow = cx.stack.filter(|s| s.continuations).map(|stack| {
        let (fields, _) = cx.frame(h).expect("a handler's frame");
        Shadow {
            stack,
            field: fields.last().expect("a segment field").0,
            outer_sp: new_local(&mut locals, ValType::I32),
            outer_limit: new_local(&mut locals, ValType::I32),
            segment: new_local(&mut locals, ValType::I32),
        }
    });
    // When counting, the frame is given back once the body is done, also
    // when an exception escapes it, so the resume is in a `try_table` then
    // too.
    let counts = cx.rt.as_ref().map(|rt| (rt.release(), rt.dup()));
    let frame_size = cx.frame(h)?.1;
    let release_frame = |code: &mut Vec<I<'static>>| {
        if let Some((release, _)) = counts {
            code.extend([
                I::LocalGet(0),
                I::I32Const(frame_size as i32),
                I::Call(release),
            ]);
        }
    };
    let catching = shadow.is_some() || counts.is_some();
    let mut table = Vec::new();
    for i in 0..n {
        let (e, j, _) = crate::clause_at(decl, i);
        let tag = cx.tag(e, j).ok_or("no tag for an operation")?;
        let label = i + u32::from(catching);
        table.push(Handle::OnLabel { tag, label });
    }

    // The frame goes on top of the handlers around this run.
    let mut code = vec![
        I::LocalGet(0),
        I::GlobalGet(HANDLERS),
        store(Storage::I32, 12),
        I::LocalGet(0),
        I::GlobalSet(HANDLERS),
    ];
    if let Some(sh) = &shadow {
        code.extend(sh.enter(start));
    }
    if catching {
        // An exception that escapes the body ends it.
        code.push(I::Block(BlockType::Result(ValType::Ref(RefType::EXNREF))));
    }
    let body_results = cx.body_results(h)?;
    let ret_block = cx.sig(FnSig {
        params: Vec::new(),
        results: body_results.clone(),
    });
    code.push(I::Block(BlockType::FunctionType(ret_block)));
    let mut op_params = Vec::new();
    for i in 0..n {
        let op = crate::op_at(m, decl, i);
        op_params.push(op.params.iter().map(val).collect::<Vec<_>>());
    }
    for i in (0..n).rev() {
        let mut results = op_params[i as usize].clone();
        results.push(nullable(cx.op_cont(h, i)?));
        let ty = cx.sig(FnSig {
            params: Vec::new(),
            results,
        });
        code.push(I::Block(BlockType::FunctionType(ty)));
    }
    if catching {
        // A catch's label is counted from outside the `try_table`.
        let catches = vec![Catch::AllRef { label: n + 1 }];
        code.push(I::TryTable(
            BlockType::FunctionType(ret_block),
            catches.into(),
        ));
    }
    match start {
        Start::Body => {
            let cont = cx.start_cont(h)?;
            code.extend([
                I::LocalGet(1 + env),
                I::RefFunc(sw.tramp),
                I::ContNew(cont),
                I::Resume {
                    cont_type_index: cont,
                    resume_table: table.into(),
                },
            ]);
        }
        Start::Resume(i) => {
            let k = 1 + env;
            let values = sig.params.len() as u32 - k - 1;
            for v in 0..values {
                code.push(I::LocalGet(k + 1 + v));
            }
            code.push(I::LocalGet(k));
            code.push(I::Resume {
                cont_type_index: cx.op_cont(h, i)?,
                resume_table: table.into(),
            });
        }
        Start::Throw(i) => {
            let k = 1 + env;
            let tag = cx.disc_tag.ok_or("no tag for discontinue")?;
            code.push(I::LocalGet(k + 1));
            code.push(I::LocalGet(k));
            code.push(I::ResumeThrow {
                cont_type_index: cx.op_cont(h, i)?,
                tag_index: tag,
                resume_table: table.into(),
            });
        }
    }
    if catching {
        code.push(I::End);
    }
    code.push(I::Br(n));
    let (kont_fields, kont_size) = cx.kont_layout(h);
    for i in 0..n {
        code.push(I::End);
        // Clause `i`, with what the `perform` gave and its continuation on
        // the stack, runs under the handlers outside this one.
        let k = new_local(&mut locals, nullable(cx.op_cont(h, i)?));
        let params: Vec<u32> = op_params[i as usize]
            .clone()
            .into_iter()
            .map(|ty| new_local(&mut locals, ty))
            .collect();
        code.push(I::LocalSet(k));
        for &p in params.iter().rev() {
            code.push(I::LocalSet(p));
        }
        // The Wasm continuation goes in a slot of its table.
        let slot = new_local(&mut locals, ValType::I32);
        let node = new_local(&mut locals, ValType::I32);
        code.extend(cx.take_slot(sw.conts[i as usize], k, slot, node));
        if let Some(sh) = &shadow {
            code.extend(sh.suspend());
        }
        code.extend(outside());
        for j in 0..env {
            code.push(I::LocalGet(1 + j));
        }
        for &p in &params {
            code.push(I::LocalGet(p));
        }
        // The Conduit continuation.
        let obj = new_local(&mut locals, ValType::I32);
        code.extend([
            I::I32Const(kont_size as i32),
            I::Call(cx.alloc),
            I::LocalSet(obj),
        ]);
        code.extend([
            I::LocalGet(obj),
            I::I32Const(sw.kont_slots[i as usize] as i32),
            store(Storage::I32, 4),
        ]);
        let disc = sw.disc_slots.get(i as usize).map_or(-1, |&s| s as i32);
        code.extend([
            I::LocalGet(obj),
            I::I32Const(disc),
            store(Storage::I32, kont_fields[0].0),
        ]);
        code.extend([
            I::LocalGet(obj),
            I::LocalGet(0),
            store(Storage::I32, kont_fields[1].0),
        ]);
        // The clause takes the environment, and the continuation keeps
        // its own.
        for j in 0..env as usize {
            let (at, s) = kont_fields[2 + j];
            code.extend([I::LocalGet(obj), I::LocalGet(1 + j as u32), store(s, at)]);
            if let Some((_, dup)) = counts
                && key_of(&decl.env[j]).is_some()
            {
                code.extend([I::LocalGet(1 + j as u32), I::Call(dup)]);
            }
        }
        let (at, s) = kont_fields[2 + env as usize];
        code.extend([I::LocalGet(obj), I::LocalGet(slot), store(s, at)]);
        if counts.is_some() {
            let (at, s) = kont_fields[3 + env as usize];
            code.extend([I::LocalGet(obj), I::I32Const(0), store(s, at)]);
        }
        code.push(I::LocalGet(obj));
        code.push(I::ReturnCall(cx.func_index(crate::clause_at(decl, i).2)?));
    }
    code.push(I::End);
    // The body returned: the return clause, under the handlers outside.
    let results: Vec<u32> = body_results
        .into_iter()
        .map(|ty| new_local(&mut locals, ty))
        .collect();
    for &r in results.iter().rev() {
        code.push(I::LocalSet(r));
    }
    if let Some(sh) = &shadow {
        code.extend(sh.leave());
    }
    code.extend(outside());
    release_frame(&mut code);
    for j in 0..env {
        code.push(I::LocalGet(1 + j));
    }
    for &r in &results {
        code.push(I::LocalGet(r));
    }
    code.push(I::ReturnCall(cx.func_index(decl.ret)?));
    if catching {
        // The exception, after the body's segment and frame are given
        // back.
        code.push(I::End);
        if let Some(sh) = &shadow {
            code.extend(sh.leave());
        }
        release_frame(&mut code);
        code.push(I::ThrowRef);
    }
    code.push(I::End);
    Ok(Body { locals, code })
}

/// The shadow stack of a continuation, in its `start`, `drive` or `throw`
/// function: its first segment and the outer stack's pointer and limit,
/// which it puts back when the continuation suspends, returns or throws.
struct Shadow {
    stack: ShadowStack,
    /// Where the frame holds the segment.
    field: u32,
    outer_sp: u32,
    outer_limit: u32,
    segment: u32,
}

impl Shadow {
    /// Saves the outer stack's pointer and limit, and switches to the
    /// continuation's: a new segment for `start`, or else where it was
    /// suspended, which its first segment's header holds.
    fn enter(&self, start: Start) -> Vec<I<'static>> {
        let (stack, at) = (self.stack, self.field);
        let mut code = shadow::save(stack, self.outer_sp, self.outer_limit).to_vec();
        match start {
            Start::Body => code.extend([
                I::I32Const((stack.segment - HEADER) as i32),
                I::Call(stack.grow),
                I::GlobalGet(stack.limit),
                I::I32Const(HEADER as i32),
                I::I32Sub,
                I::LocalSet(self.segment),
                I::LocalGet(0),
                I::LocalGet(self.segment),
                store(Storage::I32, at),
            ]),
            Start::Resume(_) | Start::Throw(_) => code.extend([
                I::LocalGet(0),
                load(Storage::I32, at),
                I::LocalTee(self.segment),
                load(Storage::I32, 0),
                I::GlobalSet(stack.sp),
                I::LocalGet(self.segment),
                load(Storage::I32, 4),
                I::GlobalSet(stack.limit),
            ]),
        }
        code
    }

    /// The outer stack's pointer and limit again.
    fn restore(&self) -> [I<'static>; 4] {
        [
            I::LocalGet(self.outer_sp),
            I::GlobalSet(self.stack.sp),
            I::LocalGet(self.outer_limit),
            I::GlobalSet(self.stack.limit),
        ]
    }

    /// Suspended: the first segment keeps the continuation's pointer and
    /// limit, which may be another segment's.
    fn suspend(&self) -> Vec<I<'static>> {
        let mut code = vec![
            I::LocalGet(self.segment),
            I::GlobalGet(self.stack.sp),
            store(Storage::I32, 0),
            I::LocalGet(self.segment),
            I::GlobalGet(self.stack.limit),
            store(Storage::I32, 4),
        ];
        code.extend(self.restore());
        code
    }

    /// Returned or thrown: the segments chained since go back, which a
    /// throw can leave, then the first one.
    fn leave(&self) -> Vec<I<'static>> {
        let mut code = vec![
            I::GlobalGet(self.stack.limit),
            I::LocalGet(self.segment),
            I::I32Const(HEADER as i32),
            I::I32Add,
            I::Call(self.stack.release),
        ];
        code.extend(shadow::free_segment(self.stack, self.segment));
        code.extend(self.restore());
        code
    }
}

/// Sets the handler stack to what's outside the frame in local 0.
fn outside() -> [I<'static>; 3] {
    [
        I::LocalGet(0),
        load(Storage::I32, 12),
        I::GlobalSet(HANDLERS),
    ]
}

/// The wrapper of the Conduit continuation of `h`'s clause `i`, or with
/// `disc`, the function that discontinues it: it reads the frame, the
/// environment and the Wasm continuation out of it, and calls the
/// handler's `drive` or `throw` function.
pub(crate) fn lower_kont(
    cx: &mut Cx<'_>,
    h: HandlerId,
    i: u32,
    disc: bool,
) -> Result<Body, String> {
    let m = cx.m;
    let decl = &m.handlers[&h];
    let env = decl.env.len();
    let args = if disc {
        1
    } else {
        crate::op_at(m, decl, i).results.len() as u32
    };
    let (fields, _) = cx.kont_layout(h);
    let sw = cx.switch(h)?.clone();
    let t = sw.conts[i as usize];
    let cont = cx.op_cont(h, i)?;
    // When counting, it takes the continuation: a used one traps, and
    // otherwise it's marked used, what it holds dupped, its slot freed,
    // and itself dropped.
    let counted = cx
        .rt
        .as_ref()
        .map(|rt| (rt.dup(), rt.drop(&DropKey::Cont, false)));
    let node = 1 + args;
    let mut code = Vec::new();
    if counted.is_some() {
        let (at, s) = fields[3 + env];
        code.extend([
            I::LocalGet(0),
            load(s, at),
            I::If(BlockType::Empty),
            I::Unreachable,
            I::End,
            I::LocalGet(0),
            I::I32Const(1),
            store(s, at),
        ]);
    }
    // The frame and the environment.
    for (j, &(at, s)) in fields[1..2 + env].iter().enumerate() {
        code.push(I::LocalGet(0));
        code.push(load(s, at));
        if let Some((dup, _)) = counted
            && j > 0
            && key_of(&decl.env[j - 1]).is_some()
        {
            code.extend([I::LocalGet(0), load(s, at), I::Call(dup)]);
        }
    }
    let (at, s) = fields[2 + env];
    code.extend([I::LocalGet(0), load(s, at), I::TableGet(t)]);
    if counted.is_some() {
        code.extend(cx.free_slot(t, cont, &[I::LocalGet(0), load(s, at)], node));
    }
    for a in 0..args {
        code.push(I::LocalGet(1 + a));
    }
    if let Some((_, drop)) = counted {
        code.extend([I::LocalGet(0), I::Call(drop)]);
    }
    let target = if disc {
        sw.throw[i as usize]
    } else {
        sw.drive[i as usize]
    };
    code.push(I::ReturnCall(target));
    code.push(I::End);
    let locals = if counted.is_some() {
        vec![ValType::I32]
    } else {
        Vec::new()
    };
    Ok(Body { locals, code })
}

/// What a Conduit continuation of `h`'s clause `i` that was never used
/// gives back when it's freed: the handler's frame, the shadow stack
/// segment the frame holds, and the slot of its Wasm continuation, with
/// local `node` for the slot's free list. What the continuation's own
/// stack holds is lost.
fn unused_kont(
    cx: &mut Cx<'_>,
    h: HandlerId,
    i: u32,
    node: u32,
) -> Result<Vec<I<'static>>, String> {
    let env = cx.m.handlers[&h].env.len();
    let (fields, _) = cx.kont_layout(h);
    let (frame_fields, frame_size) = cx.frame(h)?;
    let t = cx.switch(h)?.conts[i as usize];
    let cont = cx.op_cont(h, i)?;
    let rt = cx
        .rt
        .as_ref()
        .ok_or("a continuation is only freed when counting")?;
    let frame = fields[1].0;
    let (used, s) = fields[3 + env];
    let mut code = vec![
        I::LocalGet(0),
        load(s, used),
        I::I32Eqz,
        I::If(BlockType::Empty),
    ];
    if let Some(stack) = cx.stack.filter(|s| s.continuations)
        && cx.segment_field(h)
    {
        // Its segments, from the one it was suspended on back to its
        // first, then that one.
        let at = frame_fields.last().expect("a segment field").0;
        let segment = [
            I::LocalGet(0),
            load(Storage::I32, frame),
            load(Storage::I32, at),
        ];
        code.extend(segment.clone());
        code.push(load(Storage::I32, 4));
        code.extend(segment.clone());
        code.extend([
            I::I32Const(HEADER as i32),
            I::I32Add,
            I::Call(stack.release),
        ]);
        code.extend(segment.clone());
        code.extend([I::GlobalGet(stack.free), store(Storage::I32, 0)]);
        code.extend(segment);
        code.push(I::GlobalSet(stack.free));
    }
    code.extend([
        I::LocalGet(0),
        load(Storage::I32, frame),
        I::I32Const(frame_size as i32),
        I::Call(rt.release()),
    ]);
    let (slot, s) = fields[2 + env];
    code.extend(cx.free_slot(t, cont, &[I::LocalGet(0), load(s, slot)], node));
    code.push(I::End);
    Ok(code)
}

/// The environment's fields of a Conduit continuation of `h`, which it
/// holds a reference to each of.
fn kont_env(cx: &Cx<'_>, h: HandlerId) -> (Vec<(u32, conduit_ir::Ty)>, u32) {
    let decl = &cx.m.handlers[&h];
    let (fields, size) = cx.kont_layout(h);
    let env = (decl.env.iter().enumerate())
        .map(|(j, t)| (fields[2 + j].0, t.clone()))
        .collect();
    (env, size)
}

/// The function that frees a Conduit continuation of `h`'s clause `i`,
/// whose count is 0, and drops what it holds.
pub(crate) fn lower_kont_free(cx: &mut Cx<'_>, h: HandlerId, i: u32) -> Result<Body, String> {
    let first = unused_kont(cx, h, i, 1)?;
    let (env, size) = kont_env(cx, h);
    let rt = cx
        .rt
        .as_ref()
        .ok_or("a continuation is only freed when counting")?;
    Ok(rt.free_fields(&env, size, first, vec![ValType::I32]))
}

/// `children(p, op)` of a Conduit continuation of `h`'s clause `i`, for
/// collecting cycles: what its environment holds.
pub(crate) fn lower_kont_children(cx: &mut Cx<'_>, h: HandlerId, i: u32) -> Result<Body, String> {
    let freeing = unused_kont(cx, h, i, 2)?;
    let (env, size) = kont_env(cx, h);
    let rt = cx
        .rt
        .as_ref()
        .ok_or("a continuation is only freed when counting")?;
    Ok(rt.children_of_fields(&env, size, freeing, vec![ValType::I32]))
}
