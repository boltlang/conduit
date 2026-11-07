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
//!   Conduit continuation holds its slot. Slots are never freed, as memory
//!   isn't.
//! - A Conduit continuation is a closure: the header, its wrapper's table
//!   slot at 4, then, laid out from 8, the slot of the function that
//!   discontinues it, the frame, the handler's environment and the slot of
//!   the Wasm continuation. Its wrapper resumes it under the handler again
//!   through `drive`, so the handler is deep, and a second resume traps.
//! - `discontinue F.j(k, args)` makes a closure that performs `F.j(args)`
//!   and throws its address into `k` with `resume_throw`, which the
//!   `perform` catches and calls, under its own handlers.
//! - With `#[abi(flat)]` externs, each continuation has a shadow stack
//!   segment of its own (see [`Segments`]), which `start` takes and
//!   keeps in the frame. `start`, `drive` and `throw` switch to it, and
//!   back to the outer stack's when the body suspends, returns or throws,
//!   giving it back in the last two cases. A continuation that's never
//!   resumed again keeps its segment, as it keeps its table slot.

use conduit_ir::HandlerId;
use wasm_encoder::{BlockType, Catch, Handle, HeapType, Instruction as I, RefType, ValType};

use crate::counts::LIVE;
use crate::func::{Body, load, store};
use crate::{Cx, FnSig, HANDLERS, Segments, Storage, layout, val};

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
    /// and the slot of the Wasm continuation.
    pub fn kont_layout(&self, h: HandlerId) -> (Vec<(u32, Storage)>, u32) {
        let decl = &self.m.handlers[&h];
        let mut tys = vec![conduit_ir::Ty::I32, conduit_ir::Ty::I32];
        tys.extend(decl.env.iter().cloned());
        tys.push(conduit_ir::Ty::I32);
        layout(&tys)
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
    let segments = cx.stack.and_then(|s| Some((s.global, s.segments?)));
    let shadow = segments.map(|seg| {
        let (fields, _) = cx.frame(h).expect("a handler's frame");
        Shadow {
            seg,
            field: fields.last().expect("a segment field").0,
            outer_sp: new_local(&mut locals, ValType::I32),
            outer_limit: new_local(&mut locals, ValType::I32),
            segment: new_local(&mut locals, ValType::I32),
        }
    });
    let mut table = Vec::new();
    for i in 0..n {
        let (e, j, _) = crate::clause_at(decl, i);
        let tag = cx.tag(e, j).ok_or("no tag for an operation")?;
        let label = i + u32::from(shadow.is_some());
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
        code.extend(sh.enter(start, cx.alloc, cx.rt.is_some()));
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
    if shadow.is_some() {
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
    if shadow.is_some() {
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
        // The Wasm continuation goes in a new slot of its table.
        let slot = new_local(&mut locals, ValType::I32);
        code.extend([
            I::LocalGet(k),
            I::I32Const(1),
            I::TableGrow(sw.conts[i as usize]),
            I::LocalTee(slot),
            I::I32Const(-1),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::Unreachable,
            I::End,
        ]);
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
        for j in 0..env as usize {
            let (at, s) = kont_fields[2 + j];
            code.extend([I::LocalGet(obj), I::LocalGet(1 + j as u32), store(s, at)]);
        }
        let (at, s) = kont_fields[2 + env as usize];
        code.extend([I::LocalGet(obj), I::LocalGet(slot), store(s, at)]);
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
    for j in 0..env {
        code.push(I::LocalGet(1 + j));
    }
    for &r in &results {
        code.push(I::LocalGet(r));
    }
    code.push(I::ReturnCall(cx.func_index(decl.ret)?));
    if let Some(sh) = &shadow {
        // The exception, after the body's segment is given back.
        code.push(I::End);
        code.extend(sh.leave());
        code.push(I::ThrowRef);
    }
    code.push(I::End);
    Ok(Body { locals, code })
}

/// The shadow stack of a continuation, in its `start`, `drive` or `throw`
/// function: the segment and the outer stack's pointer and limit, which
/// it puts back when the continuation suspends, returns or throws.
struct Shadow {
    seg: (u32, Segments),
    /// Where the frame holds the segment.
    field: u32,
    outer_sp: u32,
    outer_limit: u32,
    segment: u32,
}

impl Shadow {
    /// Saves the outer stack's pointer and limit, and switches to the
    /// continuation's segment: a new one for `start`, taken from the free
    /// list or else from the heap, which isn't an object.
    fn enter(&self, start: Start, alloc: u32, counts: bool) -> Vec<I<'static>> {
        let ((sp, seg), at) = (self.seg, self.field);
        let mut code = vec![
            I::GlobalGet(sp),
            I::LocalSet(self.outer_sp),
            I::GlobalGet(seg.limit),
            I::LocalSet(self.outer_limit),
        ];
        match start {
            Start::Body => {
                code.extend([
                    I::GlobalGet(seg.free),
                    I::LocalTee(self.segment),
                    I::If(BlockType::Empty),
                    I::LocalGet(self.segment),
                    load(Storage::I32, 0),
                    I::GlobalSet(seg.free),
                    I::Else,
                    I::I32Const(seg.size as i32),
                    I::Call(alloc),
                    I::LocalSet(self.segment),
                ]);
                if counts {
                    code.extend([
                        I::GlobalGet(LIVE),
                        I::I32Const(1),
                        I::I32Sub,
                        I::GlobalSet(LIVE),
                    ]);
                }
                code.extend([
                    I::End,
                    I::LocalGet(0),
                    I::LocalGet(self.segment),
                    store(Storage::I32, at),
                    I::LocalGet(self.segment),
                    I::I32Const(seg.size as i32),
                    I::I32Add,
                    I::GlobalSet(sp),
                ]);
            }
            Start::Resume(_) | Start::Throw(_) => code.extend([
                I::LocalGet(0),
                load(Storage::I32, at),
                I::LocalTee(self.segment),
                load(Storage::I32, 0),
                I::GlobalSet(sp),
            ]),
        }
        code.extend([
            I::LocalGet(self.segment),
            I::I32Const(8),
            I::I32Add,
            I::GlobalSet(seg.limit),
        ]);
        code
    }

    /// The outer stack's pointer and limit again.
    fn restore(&self) -> [I<'static>; 4] {
        let (sp, seg) = self.seg;
        [
            I::LocalGet(self.outer_sp),
            I::GlobalSet(sp),
            I::LocalGet(self.outer_limit),
            I::GlobalSet(seg.limit),
        ]
    }

    /// Suspended: the segment keeps the continuation's pointer.
    fn suspend(&self) -> Vec<I<'static>> {
        let mut code = vec![
            I::LocalGet(self.segment),
            I::GlobalGet(self.seg.0),
            store(Storage::I32, 0),
        ];
        code.extend(self.restore());
        code
    }

    /// Returned or thrown: the segment goes on the free list.
    fn leave(&self) -> Vec<I<'static>> {
        let free = self.seg.1.free;
        let mut code = vec![
            I::LocalGet(self.segment),
            I::GlobalGet(free),
            store(Storage::I32, 0),
            I::LocalGet(self.segment),
            I::GlobalSet(free),
        ];
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
    let mut code = Vec::new();
    // The frame and the environment.
    for &(at, s) in &fields[1..2 + env] {
        code.push(I::LocalGet(0));
        code.push(load(s, at));
    }
    let (at, s) = fields[2 + env];
    code.extend([
        I::LocalGet(0),
        load(s, at),
        I::TableGet(sw.conts[i as usize]),
    ]);
    for a in 0..args {
        code.push(I::LocalGet(1 + a));
    }
    let target = if disc {
        sw.throw[i as usize]
    } else {
        sw.drive[i as usize]
    };
    code.push(I::ReturnCall(target));
    code.push(I::End);
    Ok(Body {
        locals: Vec::new(),
        code,
    })
}
