//! Effect handlers whose clauses are all tail-resumptive or aborting (see
//! [`conduit_ir::clause`]), lowered without capturing a continuation, as
//! the Wasm backend lowers them (#45). Any other handler needs evidence
//! passing first (`conduit_effects::lower_effects`).
//!
//! - `@conduit_handlers` holds the stack of handler frames. A frame is an
//!   `alloca` in the function that runs the `handle`, which is live as
//!   long as the body runs: its handler's number at 0, the next frame at
//!   8, then the handler's environment, then, for a handler with an
//!   aborting clause, its results and a buffer for `_setjmp`.
//! - `handle` pushes a frame, applies the body, pops the frame, and calls
//!   the return clause with the environment and the body's results.
//! - `perform` walks the stack to the nearest frame of a handler with a
//!   clause for its operation, trapping if there's none, and calls that
//!   clause's wrapper, `conduit_clause.<handler>.<slot>`, with the frame
//!   and its arguments, with the stack set to the frames below while the
//!   clause runs, so that what it performs goes to the handlers outside.
//!   The wrapper reads the environment out of the frame, adding a
//!   reference to each for the clause when counting, and calls the clause
//!   as a plain function, without its continuation.
//! - An aborting clause's wrapper stores what the clause gives in the
//!   frame and `_longjmp`s to the buffer, so that the `handle`'s
//!   `_setjmp` returns again, and the `handle` gives the frame's results.
//!   `_setjmp` and `_longjmp` don't save or restore the signal mask, which
//!   Conduit code never changes.
//! - A tail-resumptive clause that discontinues sets the stack to that of
//!   the `perform` it handles, which `@conduit_site` holds while a clause
//!   runs, and performs.
//!
//! An abort doesn't drop what the frames it skips own, nor the handlers'
//! environments, which the IR says dropping the continuation does: they
//! leak, as in the evaluator and Wasm (#56).

use std::borrow::Cow;
use std::collections::BTreeSet;

use conduit_ir::{EffectId, FuncId, HandlerId, Module, Ty};

use crate::counts::Kind;
use crate::ir::{Call, CallConv, Declare, FnBuilder, Function, Item, Type, Value};
use crate::{call_void, layout, ret_ty, symbol, ty};

/// The bytes a frame keeps for `_setjmp`'s `jmp_buf`, 8-aligned: more
/// than it takes on the platforms the backend targets, 200 bytes on
/// x86-64 Linux, 312 on AArch64 Linux, and less on macOS.
const JMP_BUF: u64 = 512;

/// The stack of handler frames.
pub(crate) const HANDLERS: &str = "conduit_handlers";

/// The stack of the `perform` whose clause is running, for `discontinue`.
const SITE: &str = "conduit_site";

/// What lowering the handlers of a module needs to know about it.
#[derive(Debug, Default)]
pub(crate) struct Handlers {
    /// The handlers with an aborting clause.
    aborting: BTreeSet<HandlerId>,
    /// The aborting clauses.
    aborts: BTreeSet<FuncId>,
    /// Whether a tail-resumptive clause discontinues.
    discontinues: bool,
}

/// Where a frame of a handler keeps what it holds, in bytes.
pub(crate) struct Frame {
    pub env: Vec<u64>,
    /// The results an aborting clause leaves, and `_setjmp`'s buffer.
    pub results: Vec<u64>,
    pub jmp: Option<u64>,
    /// Its size in 8-byte words.
    pub words: u64,
}

/// `m` with each clause of its handlers replaced by the plain function
/// that's called for it, and what lowering the handlers needs: an error
/// if a handler has a clause that's neither tail-resumptive nor aborting.
pub(crate) fn plain_clauses(m: &Module) -> Result<(Cow<'_, Module>, Handlers), String> {
    let mut hs = Handlers::default();
    if m.handlers.is_empty() {
        return Ok((Cow::Borrowed(m), hs));
    }
    let mut copy = m.clone();
    for (&h, decl) in &m.handlers {
        for (_, _, clause) in decl.clauses() {
            let f = (m.funcs.get(&clause)).ok_or_else(|| format!("unknown clause {clause}"))?;
            if let Some(plain) = conduit_ir::clause::plain_call(f) {
                hs.discontinues |= conduit_ir::clause::discontinues(&plain);
                copy.funcs.insert(clause, plain);
            } else if let Some(plain) = conduit_ir::clause::aborting(f) {
                copy.funcs.insert(clause, plain);
                hs.aborts.insert(clause);
                hs.aborting.insert(h);
            } else {
                return Err(format!(
                    "{h} has a clause, {clause}, that's neither tail-resumptive nor \
                     aborting: lower effects first"
                ));
            }
        }
    }
    Ok((Cow::Owned(copy), hs))
}

impl Handlers {
    /// The layout of a frame of `h`.
    pub(crate) fn frame(&self, m: &Module, h: HandlerId) -> Result<Frame, String> {
        let decl = &m.handlers[&h];
        let aborting = self.aborting.contains(&h);
        // The next frame, at 8 after the number.
        let mut tys = vec![Ty::Any(conduit_ir::Mutability::Imm)];
        tys.extend(decl.env.iter().cloned());
        if aborting {
            let ret =
                (m.funcs.get(&decl.ret)).ok_or_else(|| format!("unknown clause {}", decl.ret))?;
            tys.extend(ret.results.iter().cloned());
        }
        let (offsets, size) = layout(&tys);
        let env = offsets[1..1 + decl.env.len()].to_vec();
        let results = offsets[1 + decl.env.len()..].to_vec();
        let jmp = aborting.then_some(size);
        let bytes = size + if aborting { JMP_BUF } else { 0 };
        Ok(Frame {
            env,
            results,
            jmp,
            words: bytes / 8,
        })
    }

    pub(crate) fn discontinues(&self) -> bool {
        self.discontinues
    }

    /// The globals, declarations and wrappers handlers need, if `m` has
    /// any.
    pub(crate) fn items(&self, m: &Module, counts: bool) -> Result<Vec<Item>, String> {
        // A `perform` with no handler at all still looks.
        if m.handlers.is_empty() && m.effects.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = vec![Item::Text(format!(
            "\n@{HANDLERS} = internal global ptr null\n"
        ))];
        if self.discontinues {
            out.push(Item::Text(format!("@{SITE} = internal global ptr null\n")));
        }
        if !self.aborting.is_empty() {
            out.push(
                Declare {
                    attrs: vec!["returns_twice"],
                    ..Declare::new(Type::I32, "_setjmp", vec![Type::Ptr])
                }
                .into(),
            );
            out.push(
                Declare {
                    attrs: vec!["noreturn"],
                    ..Declare::new(Type::Void, "_longjmp", vec![Type::Ptr, Type::I32])
                }
                .into(),
            );
        }
        for (&h, decl) in &m.handlers {
            for slot in 0..decl.len() {
                out.push(self.wrapper(m, h, slot, counts)?.into());
            }
        }
        Ok(out)
    }

    /// The wrapper of clause `slot` of `h`, which `perform` calls with the
    /// frame and the operation's arguments.
    fn wrapper(
        &self,
        m: &Module,
        h: HandlerId,
        slot: usize,
        counts: bool,
    ) -> Result<Function, String> {
        let decl = &m.handlers[&h];
        let (e, i, clause) = decl.clauses().nth(slot).expect("a clause for every slot");
        let op = &m.effects[&e].ops[i as usize];
        let f = &m.funcs[&clause];
        let frame = self.frame(m, h)?;
        let mut params = vec![(Type::Ptr, "frame".to_string())];
        for (j, t) in op.params.iter().enumerate() {
            params.push((ty(t)?, format!("p{j}")));
        }
        let ret = ret_ty(&op.results)?;
        let mut b = FnBuilder::new(CallConv::Tail, ret.clone(), wrapper_name(h, slot), params);
        let fr = b.param(0);
        let mut args = Vec::new();
        for (t, &at) in decl.env.iter().zip(&frame.env) {
            let lt = ty(t)?;
            let v = b.load_at(lt.clone(), &fr, at);
            // The handler keeps its environment, and the clause gets its
            // own.
            if counts && Kind::of(t).is_some() {
                call_void(
                    &mut b,
                    Value::global("conduit_dup"),
                    vec![Type::Ptr.val(&v)],
                );
            }
            args.push(lt.val(v));
        }
        for (j, t) in op.params.iter().enumerate() {
            args.push(ty(t)?.val(b.param(1 + j)));
        }
        let Some(jmp) = frame.jmp.filter(|_| self.aborts.contains(&clause)) else {
            b.return_call(Call::tailcc(ret, symbol(clause), args).tail());
            return Ok(b.finish());
        };
        let gives = ret_ty(&f.results)?;
        let r = b.call(Call::tailcc(gives.clone(), symbol(clause), args));
        for (j, (t, &at)) in f.results.iter().zip(&frame.results).enumerate() {
            let r = r.clone().expect("a call with results");
            let v = if f.results.len() == 1 {
                r
            } else {
                b.extract_value(gives.clone(), r, j as u32)
            };
            b.store_at(ty(t)?, v, &fr, at);
        }
        let buf = b.offset(&fr, jmp);
        let args = vec![Type::Ptr.val(buf), Type::I32.val(1)];
        b.call(Call::c(Type::Void, Value::global("_longjmp"), args));
        b.unreachable();
        Ok(b.finish())
    }
}

/// The wrapper of clause `slot` of `h`.
pub(crate) fn wrapper_name(h: HandlerId, slot: usize) -> String {
    format!("conduit_clause.{}.{slot}", h.0)
}

/// The handlers with a clause for operation `i` of `e`, and its slot.
pub(crate) fn handlers_of(m: &Module, e: EffectId, i: u32) -> Vec<(HandlerId, usize)> {
    (m.handlers.iter())
        .filter_map(|(&h, d)| Some((h, d.slot(e, i)?)))
        .collect()
}

/// The global holding the stack of handler frames, and that of the
/// `perform` whose clause is running.
pub(crate) fn handlers_global() -> Value {
    Value::global(HANDLERS)
}

pub(crate) fn site_global() -> Value {
    Value::global(SITE)
}
