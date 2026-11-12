//! Reference counts, made explicit: [`insert_counts`] adds the `dup`s and
//! `drop`s that "Reference counts" in `docs/ir/ir.md` asks for, so that a
//! program frees every object once nothing refers to it any more, except in
//! cycles.
//!
//! It runs once, after optimization: the other passes don't keep counts
//! right. Every variable that holds an object owns one reference to it:
//!
//! - **Parameters** are owned, of functions and of join points, except the
//!   function parameters that [`borrows`] finds are borrowed: the caller
//!   keeps the reference, so the function never drops it, and `dup`s it
//!   for each use that takes one.
//! - **What an op gives** is owned, except what `get`, `array_get`,
//!   `ref_get` and `unbox` read out of an object, which is borrowed from
//!   it: a `dup` right after makes it owned, as Perceus does. Specializing
//!   `drop`s later cancels most of those against the `drop` of the object.
//! - **A use that takes a reference** (a field of `new`, a call's argument,
//!   a jump's) needs a `dup` before it if the variable is still used
//!   afterwards, or appears again among what the op takes. Its last use
//!   gives the reference away.
//! - **After the last use** that doesn't take the reference, the variable
//!   is dropped, and so is one whose value is never used. A branch, an arm
//!   of a `case` or the body of a `join`, drops what's owned when it starts
//!   but it doesn't use.
//!
//! A join point is like a local function, and the variables its body uses
//! from outside are owned by it: each jump to it hands them over with its
//! arguments, so they stay alive at every jump.
//!
//! Variables bound by `const` or `undefined` are never counted, since
//! their counts would do nothing.
//!
//! [`insert_reuse`] runs first: where an arm of a `case` takes an object
//! apart and builds one of the same shape, the new one goes in the old
//! one's memory when that's unique, instead of freeing one and allocating
//! the other: Perceus's "functional but in-place".
//!
//! [`specialize_drops`] then makes the counts cheaper where a field read
//! out of an object is kept and the object dropped: when the object turns
//! out to be unique at run time, its reference to the field becomes the
//! variable's, so neither is counted.

use std::collections::{BTreeMap, BTreeSet};

use conduit_ir::types::resolve;
use conduit_ir::{
    Arm, Atom, Block, CtorId, DataDecl, DataId, Func, FuncId, JoinDef, Label, Let, Module,
    Mutability, Op, Tag, Tail, Ty, Var,
};

type Set = BTreeSet<Var>;

mod borrows;
mod counts;
mod reuse;
mod specialize;

pub use borrows::*;
pub use counts::*;
pub use reuse::*;
pub use specialize::*;
