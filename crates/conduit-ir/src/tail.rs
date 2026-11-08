//! Tail calls: a `let` whose results go straight back to the caller. See
//! "Jumps are tail calls by construction" in `docs/ir.md`.
//!
//! A call is a tail call when the `jump` after it returns exactly what it
//! binds, `let %r = call f(..); jump ret(%r)`, or hands it to a join point
//! that only passes its parameters on in the same way, like the
//! `join j1(%x) = jump ret(%x)` that a frontend makes for the value of an
//! `if`. Then nothing of the caller is left to run, and the call can
//! replace it. The evaluator, the Wasm backend, its escape analysis and
//! borrow inference use this one definition, so they agree on which calls
//! keep the stack from growing. Lowering effects keeps to `jump ret`
//! alone, since it decides which join points to lift before lifting
//! changes the jumps to them, and it's only an optimization there.

use crate::{Atom, Block, JoinDef, Label, Tail, Var};

/// The join definitions of a block, by label number: a table of
/// `labels` slots, as many as a function has labels.
pub fn join_defs(body: &Block, labels: u32) -> Vec<Option<&JoinDef>> {
    let mut table = vec![None; labels as usize];
    let mut todo = vec![body];
    while let Some(block) = todo.pop() {
        match &block.tail {
            Tail::Join { defs, body, .. } => {
                for def in defs {
                    if let Some(slot) = table.get_mut(def.label.0 as usize) {
                        *slot = Some(def);
                    }
                    todo.push(&def.body);
                }
                todo.push(body);
            }
            Tail::Case { arms, default, .. } => {
                todo.extend(arms.iter().map(|arm| &arm.body));
                todo.extend(default.as_deref());
            }
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
        }
    }
    table
}

/// Whether `tail`, right after a `let` that binds `binds`, only returns
/// them: it jumps to `ret` with exactly `binds`, in order, or to a join
/// point that only does the same with its parameters, through any number
/// of those. `defs` is [`join_defs`] of the function.
pub fn returns(binds: &[Var], tail: &Tail, defs: &[Option<&JoinDef>]) -> bool {
    let mut want: &[Var] = binds;
    let mut tail = tail;
    // A recursive join point that only jumps to itself never returns, so
    // following one can't end: stop after a jump per label.
    for _ in 0..=defs.len() {
        let Tail::Jump { target, args, .. } = tail else {
            return false;
        };
        let passes_on =
            args.len() == want.len() && args.iter().zip(want).all(|(a, v)| *a == Atom::Var(*v));
        if !passes_on {
            return false;
        }
        if *target == Label::RET {
            return true;
        }
        let Some(Some(def)) = defs.get(target.0 as usize) else {
            return false;
        };
        if !def.body.lets.is_empty() {
            return false;
        }
        want = &def.params;
        tail = &def.body.tail;
    }
    false
}
