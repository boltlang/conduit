//! Handlers that switch stacks, with the stack switching proposal: those
//! with a clause that's neither tail-resumptive nor aborting, so that its
//! continuation has to be captured.
//!
//! Each operation of an effect such a handler handles has a control tag.
//! `handle` makes a frame of the handler's own type and calls the
//! handler's `start` function, which links the frame onto the handler
//! stack, runs the body as a Wasm continuation with a `resume` that has a
//! label for each operation's tag, and then:
//!
//! - if the body returns, calls the return clause, under the handlers
//!   outside this one;
//! - if it suspends with operation `i`, calls clause `i`, under the
//!   handlers outside this one, with a Conduit continuation holding the
//!   frame, the environment and the Wasm continuation.
//!
//! `perform` finds the nearest frame of its effect on the handler stack as
//! for any handler. If it's one of these, it suspends with the
//! operation's tag, which goes to the `resume` of this handler, since the
//! handlers between them are of other effects. Once it's resumed, it puts
//! its handler stack back.
//!
//! Resuming the Conduit continuation calls its wrapper, which calls the
//! handler's `drive` function for the operation: the same as `start`, but
//! resuming the Wasm continuation with the operation's results, so the
//! handler is deep. A Wasm continuation can only be resumed once, so a
//! second resume traps, as rule 7 says. The frame's `next` is set on every
//! `start` or `drive`, so the body sees the handlers around wherever it
//! was last resumed.
//!
//! `discontinue F.j(k, args)` makes a closure that performs `F.j(args)`,
//! and the continuation's `disc` function throws it into the Wasm
//! continuation with `resume_throw`, through the handler's `throw`
//! function. The `perform` the continuation was captured at catches it,
//! puts its handler stack back, and calls it: so `F.j` is performed where
//! the continuation was captured, under the handlers around it, and what
//! it gives is what the `perform` gives.

use conduit_ir::{EffectId, HandlerId};
use wasm_encoder::{BlockType, Handle, Instruction as I, ValType};

use crate::func::Body;
use crate::types::{FnSig, nullable};
use crate::{Cx, HANDLERS};

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

/// The signature of the `start`, `drive` or `throw` function of `h`: the
/// frame, the environment, then what `start` says, to what the `handle`
/// gives.
pub(crate) fn drive_sig(cx: &mut Cx<'_>, h: HandlerId, start: Start) -> Result<FnSig, String> {
    let m = cx.m;
    let decl = &m.handlers[&h];
    let mut params = vec![nullable(cx.types.switch_frame()?)];
    params.extend(cx.types.vals(&decl.env)?);
    match start {
        Start::Body => {
            let body = FnSig {
                params: Vec::new(),
                results: cx.types.body_results(h)?,
            };
            params.push(nullable(cx.types.closure(&body)?));
        }
        Start::Resume(i) => {
            params.push(nullable(cx.types.op_cont(h, i)?));
            params.extend(cx.types.clause_cont_sig(h, i)?.params);
        }
        Start::Throw(i) => {
            params.push(nullable(cx.types.op_cont(h, i)?));
            params.push(ValType::Ref(wasm_encoder::RefType::ANYREF));
        }
    }
    let Some(ret) = m.funcs.get(&decl.ret) else {
        return Err(format!("unknown return clause {}", decl.ret));
    };
    Ok(FnSig {
        params,
        results: cx.types.vals(&ret.results)?,
    })
}

/// The `start`, `drive` or `throw` function of `h`.
pub(crate) fn lower_drive(cx: &mut Cx<'_>, h: HandlerId, start: Start) -> Result<Body, String> {
    let m = cx.m;
    let decl = &m.handlers[&h];
    let Some(effect) = m.effects.get(&decl.effect) else {
        return Err(format!("unknown effect {}", decl.effect));
    };
    let n = effect.ops.len() as u32;
    let sig = drive_sig(cx, h, start)?;
    let env = decl.env.len() as u32;
    let switch = cx
        .switch(h)
        .map(|s| (s.entry.clone(), s.disc.clone()))
        .ok_or_else(|| format!("{h} doesn't switch stacks"))?;
    let base = cx.types.frame()?;
    let mut locals: Vec<ValType> = Vec::new();
    let first_local = sig.params.len() as u32;
    let new_local = |locals: &mut Vec<ValType>, ty: ValType| {
        locals.push(ty);
        first_local + locals.len() as u32 - 1
    };
    let table: Vec<Handle> = (0..n)
        .map(|i| {
            let tag = cx.tag(decl.effect, i).expect("a tag for each operation");
            Handle::OnLabel { tag, label: i }
        })
        .collect();

    // The frame goes on top of the handlers around this run.
    let mut code = vec![
        I::LocalGet(0),
        I::GlobalGet(HANDLERS),
        I::StructSet {
            struct_type_index: base,
            field_index: 1,
        },
        I::LocalGet(0),
        I::GlobalSet(HANDLERS),
    ];
    let body_results = cx.types.body_results(h)?;
    let ret_block = cx.types.func(&FnSig {
        params: Vec::new(),
        results: body_results.clone(),
    })?;
    code.push(I::Block(BlockType::FunctionType(ret_block)));
    // Operation `i` lands at label `i`, with its parameters and the
    // continuation.
    let mut op_params = Vec::new();
    for i in 0..n {
        let op = &effect.ops[i as usize];
        op_params.push(cx.types.vals(&op.params)?);
    }
    for i in (0..n).rev() {
        let mut results = op_params[i as usize].clone();
        results.push(nullable(cx.types.op_cont(h, i)?));
        let ty = cx.types.func(&FnSig {
            params: Vec::new(),
            results,
        })?;
        code.push(I::Block(BlockType::FunctionType(ty)));
    }
    match start {
        Start::Body => {
            let body = FnSig {
                params: Vec::new(),
                results: body_results.clone(),
            };
            let closure = cx.types.closure(&body)?;
            let cont = cx.types.cont_of_entry(&body)?;
            let c = 1 + env;
            code.extend([
                I::LocalGet(c),
                I::LocalGet(c),
                I::StructGet {
                    struct_type_index: closure,
                    field_index: 0,
                },
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
                cont_type_index: cx.types.op_cont(h, i)?,
                resume_table: table.into(),
            });
        }
        Start::Throw(i) => {
            let k = 1 + env;
            let tag = cx.disc_tag.ok_or("no tag for discontinue")?;
            code.push(I::LocalGet(k + 1));
            code.push(I::LocalGet(k));
            code.push(I::ResumeThrow {
                cont_type_index: cx.types.op_cont(h, i)?,
                tag_index: tag,
                resume_table: table.into(),
            });
        }
    }
    code.push(I::Br(n));
    for i in 0..n {
        code.push(I::End);
        // Clause `i`, with what the `perform` gave and its continuation on
        // the stack, runs under the handlers outside this one.
        let (cont_of, _) = cx.types.cont_of(h, i)?;
        let k = new_local(&mut locals, nullable(cx.types.op_cont(h, i)?));
        let params: Vec<u32> = op_params[i as usize]
            .clone()
            .into_iter()
            .map(|ty| new_local(&mut locals, ty))
            .collect();
        code.push(I::LocalSet(k));
        for &p in params.iter().rev() {
            code.push(I::LocalSet(p));
        }
        code.extend(outside(base));
        for j in 0..env {
            code.push(I::LocalGet(1 + j));
        }
        for &p in &params {
            code.push(I::LocalGet(p));
        }
        code.push(I::RefFunc(switch.0[i as usize]));
        if let Some(&disc) = switch.1.get(i as usize) {
            code.push(I::RefFunc(disc));
        }
        code.push(I::LocalGet(0));
        for j in 0..env {
            code.push(I::LocalGet(1 + j));
        }
        code.push(I::LocalGet(k));
        code.push(I::StructNew(cont_of));
        code.push(I::ReturnCall(cx.func_index(decl.ops[i as usize])?));
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
    code.extend(outside(base));
    for j in 0..env {
        code.push(I::LocalGet(1 + j));
    }
    for &r in &results {
        code.push(I::LocalGet(r));
    }
    code.push(I::ReturnCall(cx.func_index(decl.ret)?));
    code.push(I::End);
    Ok(Body {
        locals,
        code,
        local_names: Vec::new(),
    })
}

/// Sets the handler stack to what's outside the frame in local 0.
fn outside(base: u32) -> [I<'static>; 3] {
    [
        I::LocalGet(0),
        I::StructGet {
            struct_type_index: base,
            field_index: 1,
        },
        I::GlobalSet(HANDLERS),
    ]
}

/// The wrapper of the Conduit continuation clause `i` of `h` gets, or with
/// `disc`, the function it discontinues with: it reads the frame, the
/// environment and the Wasm continuation out of it, and calls the
/// handler's `drive` or `throw` function.
pub(crate) fn lower_cont_entry(
    cx: &mut Cx<'_>,
    h: HandlerId,
    i: u32,
    disc: bool,
) -> Result<Body, String> {
    let env = cx.m.handlers[&h].env.len() as u32;
    let sig = cx.types.clause_cont_sig(h, i)?;
    let (k, first) = cx.types.cont_of(h, i)?;
    let args = if disc { 1 } else { sig.params.len() as u32 };
    let s = 1 + args;
    let mut code = vec![
        I::LocalGet(0),
        I::RefCastNullable(wasm_encoder::HeapType::Concrete(k)),
        I::LocalSet(s),
    ];
    for f in 0..env + 2 {
        code.push(I::LocalGet(s));
        code.push(I::StructGet {
            struct_type_index: k,
            field_index: first + f,
        });
    }
    for a in 0..args {
        code.push(I::LocalGet(1 + a));
    }
    let switch = cx
        .switch(h)
        .ok_or_else(|| format!("{h} doesn't switch stacks"))?;
    let target = if disc {
        switch.throw[i as usize]
    } else {
        switch.drive[i as usize]
    };
    code.push(I::ReturnCall(target));
    code.push(I::End);
    Ok(Body {
        locals: vec![nullable(k)],
        code,
        local_names: Vec::new(),
    })
}

/// The wrapper of the closure that `discontinue E.i` throws: it reads the
/// operation's arguments out of it and calls the function that performs
/// it.
pub(crate) fn lower_thunk_entry(cx: &mut Cx<'_>, e: EffectId, i: u32) -> Result<Body, String> {
    let (t, _) = cx.types.thunk(e, i)?;
    let args = cx.m.effects[&e].ops[i as usize].params.len() as u32;
    let mut code = vec![
        I::LocalGet(0),
        I::RefCastNullable(wasm_encoder::HeapType::Concrete(t)),
        I::LocalSet(1),
    ];
    for a in 0..args {
        code.push(I::LocalGet(1));
        code.push(I::StructGet {
            struct_type_index: t,
            field_index: 1 + a,
        });
    }
    let (body, _) = cx.thunks[&(e, i)];
    code.push(I::ReturnCall(body));
    code.push(I::End);
    Ok(Body {
        locals: vec![nullable(t)],
        code,
        local_names: Vec::new(),
    })
}
