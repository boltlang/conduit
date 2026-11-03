//! An evaluator for Conduit IR.
//!
//! It only accepts verified IR (see [`conduit_ir::verify`]). It doesn't
//! check types again while running: IR the verifier would reject gives an
//! [`ErrorKind::Unverified`] error, or a wrong result, but never a panic.
//!
//! - **No recursion for control flow.** The machine runs a loop over an
//!   explicit stack of frames. Every term is in tail position, so a `let`,
//!   a jump or a `case` replaces the current term, and loops don't grow the
//!   stack. Calls push a frame. Tail calls (`let r = call f(..); jump ret(r)`)
//!   and tail resumes replace the caller's frame.
//! - **Handlers move frames.** `handle` pushes a handler frame. `perform`
//!   moves every frame from the nearest handler of its effect up into a
//!   continuation, and `resume` moves them back, so resuming twice finds no
//!   frames and gives [`ErrorKind::ContinuationResumedTwice`].
//! - **Objects** are in a heap and are never freed yet. Each one that can
//!   be `Imm` or `Mut` knows which it is, so that a write to an `Imm` object
//!   is an error ([`ErrorKind::WriteToImm`]) rather than going unnoticed.
//! - **Numbers** follow Rust for now: integers wrap, shifts take the amount
//!   modulo the width, and floats use Rust's operations and `as`
//!   conversions. Integer division and remainder by zero, and division of
//!   the smallest integer by -1, are errors. The IR doesn't define these
//!   cases precisely yet.
//! - **Undefined behavior** of the IR, like `Unbox` at a type the value
//!   wasn't boxed at, is an [`ErrorKind::Undefined`] error.

mod error;
mod machine;
mod prim;
mod value;

pub use error::{Error, ErrorKind};
pub use machine::{Host, Machine, NoHost};
pub use value::{Continuation, ObjId, Object, Value};

#[cfg(test)]
mod tests;
