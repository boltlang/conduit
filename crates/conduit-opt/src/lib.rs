//! Optimization passes on Conduit IR. `docs/optimisations.md` gives the
//! design, "A few rewrites", and lists them one by one.
//!
//! A [`Pass`] rewrites one function at a time and reaches other items only
//! through a [`Program`], so a frontend can optimize each function
//! separately and cache the results. Passes are deterministic: the same
//! input always gives the same output.
//!
//! Every pass takes verified IR and gives verified IR.
//!
//! [`optimize_func`] runs [`PASSES`] until none of them changes anything,
//! and [`optimize_module`] does that for every function of a module.

pub mod closures;
mod compact;
pub mod copies;
mod copy;
pub mod cse;
pub mod dce;
pub mod direct;
pub mod handlers;
pub mod inline;
pub mod joins;
pub mod ownership;
pub mod promote;
pub mod simplify;
pub mod sroa;

use conduit_ir::{Func, FuncId, Module, Program};

/// The largest function a pass copies into another, like a callee that
/// `inline` inlines, in `let`s and tails.
pub const MAX_CALLEE_SIZE: usize = 20;

/// A pass stops copying into a function once it's this big, in `let`s and
/// tails, so that a cycle of calls can't make it grow without end.
pub const MAX_CALLER_SIZE: usize = 1000;

/// A rewrite of one function.
pub trait Pass {
    /// A short name, for debugging and for choosing passes.
    fn name(&self) -> &'static str;

    /// Rewrites `f`, the function `id` of `p`. Returns whether anything
    /// changed. `p` may still hold the old version of `f`.
    fn run(&self, p: &dyn Program, id: FuncId, f: &mut Func) -> bool;
}

/// The passes [`optimize_func`] runs, in order. Handlers their body can't
/// use go first, since what's left of one applies a closure. Known
/// closures go next,
/// since the calls they make can then be inlined in the same round.
/// Inlining is next, since the others only see one function, and a
/// callee's body in its caller shows them more. Handlers run directly right
/// after, since inlining is what puts a `perform` where its handler's body
/// is the function itself. Scalar replacement turns
/// objects that don't escape into refs, which promoting refs, next, turns
/// into values. Promoting refs comes early, since a ref holding an
/// array ties it to the ref's region, which keeps `copies` from freezing
/// it, and since it turns what refs hold into values that `simplify` can
/// see. Join points are inlined right before `simplify`, which picks the
/// arm of a `case` on a value a jump gave. `simplify` leaves code behind for dead code to remove, and so does
/// `cse`, which goes after it since folding makes more ops look alike. Dead code
/// goes before `copies`, since a dead use of a value can keep it from
/// freezing.
pub const PASSES: &[&dyn Pass] = &[
    &handlers::Handlers,
    &closures::KnownClosures,
    &inline::Inline,
    &direct::Direct,
    &sroa::Sroa,
    &promote::Promote,
    &joins::Joins,
    &simplify::Simplify,
    &cse::Cse,
    &dce::Dce,
    &copies::Copies,
];

/// Runs the passes over and over at most this many times, in case some
/// pair of them keeps undoing each other.
const MAX_ROUNDS: usize = 16;

/// Runs [`PASSES`] on `f` until none of them changes it. Returns whether
/// anything changed.
pub fn optimize_func(p: &dyn Program, id: FuncId, f: &mut Func) -> bool {
    run_passes(PASSES, p, id, f)
}

/// Runs `passes` on `f` until none of them changes it. Returns whether
/// anything changed.
pub fn run_passes(passes: &[&dyn Pass], p: &dyn Program, id: FuncId, f: &mut Func) -> bool {
    let mut changed = false;
    for _ in 0..MAX_ROUNDS {
        let mut round = false;
        for pass in passes {
            round |= pass.run(p, id, f);
        }
        if !round {
            break;
        }
        changed = true;
    }
    if changed {
        compact::compact(f);
    }
    changed
}

/// Runs [`optimize_func`] on every function of `m`, in order of their IDs,
/// so each function sees the ones before it already optimized. Returns
/// whether anything changed.
pub fn optimize_module(m: &mut Module) -> bool {
    run_passes_on_module(PASSES, m)
}

/// The pass in [`PASSES`] with this [`Pass::name`].
pub fn pass(name: &str) -> Option<&'static dyn Pass> {
    PASSES.iter().copied().find(|p| p.name() == name)
}

/// Like [`optimize_module`], with `passes` instead of [`PASSES`].
pub fn run_passes_on_module(passes: &[&dyn Pass], m: &mut Module) -> bool {
    let mut changed = false;
    let ids: Vec<FuncId> = m.funcs.keys().copied().collect();
    for id in ids {
        let mut f = m.funcs[&id].clone();
        if run_passes(passes, m, id, &mut f) {
            m.funcs.insert(id, f);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests;
