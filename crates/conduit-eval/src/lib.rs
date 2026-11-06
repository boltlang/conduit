//! An evaluator for Conduit IR.
//!
//! It only accepts verified IR (see [`conduit_ir::verify`]). It doesn't
//! check types again while running: IR the verifier would reject gives an
//! [`ErrorKind::Unverified`] error, or a wrong result, but never a panic.
//!
//! - **No recursion for control flow.** The machine runs a loop over an
//!   explicit stack of frames. A frame holds a place in a block: a `let`
//!   moves it forward, and a jump or a `case` moves it to another block, so
//!   loops don't grow the stack. Calls push a frame. Tail calls (`let r = call f(..); jump ret(r)`)
//!   and tail resumes replace the caller's frame.
//! - **Handlers move frames.** `handle` pushes a handler frame. `perform`
//!   moves every frame from the nearest handler of its effect up into a
//!   continuation, and `resume` moves them back, so resuming twice finds no
//!   frames and gives [`ErrorKind::ContinuationResumedTwice`].
//! - **Objects** are in a heap. With [`Machine::counting`] on, each has a
//!   reference count, `dup` and `drop` change it, and an object is freed
//!   at 0; [`Machine::leaks`] reports what a run left behind. Otherwise
//!   nothing is ever freed. Each object that can be `Imm` or `Mut` knows
//!   which it is, so that a write to an `Imm` object is an error
//!   ([`ErrorKind::WriteToImm`]) rather than going unnoticed.
//! - **Numbers** follow Rust for now: integers wrap, shifts take the amount
//!   modulo the width, and floats use Rust's operations and `as`
//!   conversions, except that `min` and `max` follow Wasm (NaN if either
//!   is NaN, and `-0.0` is less than `0.0`). Integer division by zero gives
//!   0, and the remainder is then the dividend. Division of the smallest
//!   integer by -1, and converting a float that's NaN or out of range to an
//!   integer, are errors, as they trap in Wasm.
//! - **Undefined behavior** of the IR, like `Unbox` at a type the value
//!   wasn't boxed at, is an [`ErrorKind::Undefined`] error.

mod error;
mod machine;
mod prim;
mod value;

pub use error::{Error, ErrorKind};
pub use machine::{Count, Host, Leaks, Machine, NoHost, Stats};
pub use prim::fold;
pub use value::{Continuation, ObjId, Object, Value};

#[cfg(test)]
mod tests;
