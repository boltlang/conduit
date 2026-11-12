//! Effect handlers whose clauses are all tail-resumptive or aborting (see
//! [`conduit_ir::clause`]), lowered without capturing a continuation, as
//! the Wasm backend lowers them (#45). Any other handler needs evidence
//! passing first (`conduit_effects::lower_effects`).
//!
//! - `@conduit_handlers` holds the stack of handler frames. A frame is an
//!   `alloca` in the function that runs the `handle`, which is live as
//!   long as the body runs: its handler's number at 0, the next frame at
//!   8, then the handler's environment, then, for a handler with an
//!   aborting clause, its results and an exception for the unwinder, an
//!   `_Unwind_Exception`.
//! - `handle` pushes a frame, applies the body, pops the frame, and calls
//!   the return clause with the environment and the body's results. For
//!   a handler with an aborting clause, it `invoke`s the body, with a
//!   landing pad that runs only when a clause aborts.
//! - `perform` walks the stack to the nearest frame of a handler with a
//!   clause for its operation, trapping if there's none, and calls that
//!   clause's wrapper, `conduit_clause.<handler>.<slot>`, with the frame
//!   and its arguments, with the stack set to the frames below while the
//!   clause runs, so that what it performs goes to the handlers outside.
//!   The wrapper reads the environment out of the frame, adding a
//!   reference to each for the clause when counting, and calls the clause
//!   as a plain function, without its continuation.
//! - An aborting clause's wrapper stores what the clause gives in the
//!   frame and unwinds to its `handle` with the frame's exception, by
//!   `_Unwind_ForcedUnwind`, from libgcc_s or libunwind, which C programs
//!   on Linux and macOS link. A forced unwind runs only the landing pads,
//!   with no search for a handler first, so `__gcc_personality_v0`, C's
//!   personality, from the same library, does: Conduit needs no
//!   personality of its own. Each `handle` the unwinding reaches checks
//!   whether the exception is its frame's, and goes on unwinding if not;
//!   the one it is gives the frame's results. A `handle` nothing aborts
//!   to costs nothing more than one of a handler that doesn't abort.
//! - A tail-resumptive clause that discontinues sets the stack to that of
//!   the `perform` it handles, which `@conduit_site` holds while a clause
//!   runs, and performs.
//!
//! When counting, an abort drops what the IR says dropping the
//! continuation would (#56): each call that may perform, in a function
//! whose row isn't empty, while the frame owns something, is an `invoke`
//! with a landing pad that drops that (see `conduit_ir::owned`) and goes
//! on unwinding, and a `handle` that's unwound, or aborted to, drops its
//! handler's environment.

use std::borrow::Cow;
use std::collections::BTreeSet;

use conduit_ir::{EffectId, FuncId, HandlerId, Module, Ty};

use crate::counts::Kind;
use crate::ir::{BinOp, Call, CallConv, Declare, FnBuilder, Function, IntPred, Item, Type, Value};
use crate::{call_void, layout, ret_ty, symbol, ty};

/// The bytes of an `_Unwind_Exception`, which is 16-aligned, on x86-64
/// and AArch64: its class, its cleanup function and two words the
/// unwinder keeps.
const EXCEPTION: u64 = 32;

/// The class of Conduit's exceptions, which the unwinder only keeps:
/// "CONDUIT\0".
const CLASS: i64 = 0x434f_4e44_5549_5400;

/// The function `_Unwind_ForcedUnwind` calls at each frame, which goes
/// on, and traps if it reaches the end of the stack, which can't happen:
/// a clause only aborts to a `handle` that's running.
const STOP: &str = "conduit_stop";

/// C's personality, which runs cleanup landing pads.
pub(crate) const PERSONALITY: &str = "__gcc_personality_v0";

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
    /// The results an aborting clause leaves, and the exception it
    /// unwinds with, 16-aligned.
    pub results: Vec<u64>,
    pub exception: Option<u64>,
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
        let exception = aborting.then_some(size.next_multiple_of(16));
        let bytes = exception.map_or(size, |at| at + EXCEPTION);
        Ok(Frame {
            env,
            results,
            exception,
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
                Declare::new(
                    Type::I32,
                    "_Unwind_ForcedUnwind",
                    vec![Type::Ptr, Type::Ptr, Type::Ptr],
                )
                .into(),
            );
            out.push(stop_fn().into());
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
        let Some(exn) = frame.exception.filter(|_| self.aborts.contains(&clause)) else {
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
        let exn = b.offset(&fr, exn);
        b.store(Type::I64, Value::Int(CLASS), &exn);
        // No cleanup function: nothing frees the exception, which is in
        // the frame.
        b.store_at(Type::Ptr, Value::Null, &exn, 8);
        let args = vec![
            Type::Ptr.val(exn),
            Type::Ptr.val(Value::global(STOP)),
            Type::Ptr.val(Value::Null),
        ];
        // It only comes back if it fails.
        b.call(Call::c(
            Type::I32,
            Value::global("_Unwind_ForcedUnwind"),
            args,
        ));
        b.call(Call::c(Type::Void, Value::global("llvm.trap"), vec![]));
        b.unreachable();
        Ok(b.finish())
    }
}

/// [`STOP`]: `_URC_NO_REASON`, 0, to go on, unless the actions have
/// `_UA_END_OF_STACK`, 16.
fn stop_fn() -> Function {
    let params = [
        Type::I32,
        Type::I32,
        Type::I64,
        Type::Ptr,
        Type::Ptr,
        Type::Ptr,
    ];
    let params = (params.into_iter().enumerate())
        .map(|(i, t)| (t, format!("p{i}")))
        .collect();
    let mut b = FnBuilder::new(CallConv::C, Type::I32, STOP, params);
    let actions = b.param(1);
    let end = b.bin(BinOp::And, Type::I32, actions, 16);
    let end = b.icmp(IntPred::Ne, Type::I32, end, 0);
    let (trap, go) = (b.new_block(), b.new_block());
    b.cond_br(end, &trap, &go);
    b.start(&trap);
    b.call(Call::c(Type::Void, Value::global("llvm.trap"), vec![]));
    b.unreachable();
    b.start(&go);
    b.ret(Some(Type::I32.val(0)));
    b.finish()
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
