//! Building objects in the memory of those just dropped. See
//! [`insert_reuse`].

use super::*;

/// Adds reuse to every function of `m`, before [`insert_counts`]: in an arm
/// `C =>` of a `case` on `x` whose last use of `x` is in the arm's own
/// block, right after it, and before the first `new C'(..)` after it of a
/// constructor of the same shape (see
/// [`same_shape`](conduit_ir::types::same_shape)), or, if there's none in
/// the block, at the start of each branch after it that has one:
///
/// ```text
/// let %t: reuse C = drop_reuse(%x)
/// ..
/// let %y = reuse C'(%t, ..)
/// ```
///
/// That builds `y` in `x`'s memory if `x` was unique, and allocates
/// otherwise. The `drop_reuse` is right after the last use, rather than at
/// the `new`, so that `x` doesn't keep its fields alive in between: a
/// recursive call on a field, as in `map`, then finds it unique. Only in
/// the arm's own block, so that the token is always used. `x` is then
/// owned where it was borrowed, which [`borrows`] sees.
pub fn insert_reuse(m: &mut Module) {
    let shapes = Module {
        data: m.data.clone(),
        ..Module::default()
    };
    for f in m.funcs.values_mut() {
        reuse_func(&shapes, f);
    }
}

pub(super) fn reuse_func(shapes: &Module, f: &mut Func) {
    let none = Borrows::default();
    let mut cx = Counts {
        counted: f.vars.iter().map(Ty::is_object).collect(),
        lent: Set::new(),
        borrows: &none,
        free: BTreeMap::new(),
    };
    cx.find_free(&f.body);
    let mut todo = vec![&mut f.body];
    let vars = &mut f.vars;
    while let Some(b) = todo.pop() {
        match &mut b.tail {
            Tail::Case { scrut, arms, .. } => {
                for arm in arms.iter_mut() {
                    if let (Atom::Var(x), Tag::Ctor(c)) = (&*scrut, arm.tag)
                        && matches!(vars[x.0 as usize], Ty::Data(..))
                    {
                        reuse_arm(&cx, shapes, &mut arm.body, *x, c, vars);
                    }
                }
            }
            Tail::Join { .. } | Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
        }
        todo.extend(b.children_mut());
    }
}

/// Reuses `x`, built by `c`, for the first `new` of the same shape in `b`
/// after `x`'s last use, if there's one.
pub(super) fn reuse_arm(
    cx: &Counts<'_>,
    shapes: &Module,
    b: &mut Block,
    x: Var,
    c: CtorId,
    vars: &mut Vec<Ty>,
) {
    let mut afters = vec![Set::new(); b.lets.len()];
    let mut live = cx.live_tail(&b.tail);
    for (i, l) in b.lets.iter().enumerate().rev() {
        afters[i] = live.clone();
        cx.step_back(l, &mut live);
    }
    // Where `x` is dead from: after its last use, if it's in the block.
    // A last use that gives `x` away leaves no memory to reuse. If `x` is
    // still live then, the tail uses it, and its branches can still reuse
    // it where they don't.
    let gives =
        |l: &Let| (l.args.iter().enumerate()).any(|(p, a)| *a == Atom::Var(x) && takes(&l.op, p));
    let from = match b.lets.iter().rposition(|l| l.args.contains(&Atom::Var(x))) {
        Some(i) if gives(&b.lets[i]) => return,
        Some(i) if !afters[i].contains(&x) => Some(i + 1),
        None if !live.contains(&x) => Some(0),
        _ => None,
    };
    let found = from.and_then(|from| {
        let at = b.lets[from..].iter().position(|l| match l.op {
            Op::New(c2) => conduit_ir::types::same_shape(shapes, c, c2),
            _ => false,
        });
        at.map(|j| (from, from + j))
    });
    let Some((from, j)) = found else {
        // None here, so a branch after this block's last use of `x` that
        // builds one can reuse it, at its start, if it doesn't use `x`
        // itself: each arm of a `case`, and the body of a `join`, but not a
        // definition, which a loop can run more than once.
        match &mut b.tail {
            Tail::Case { arms, default, .. } => {
                for arm in arms {
                    reuse_arm(cx, shapes, &mut arm.body, x, c, vars);
                }
                if let Some(d) = default {
                    reuse_arm(cx, shapes, d, x, c, vars);
                }
            }
            Tail::Join { body, .. } => reuse_arm(cx, shapes, body, x, c, vars),
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
        }
        return;
    };
    vars.push(Ty::Reuse(c));
    let t = Var(vars.len() as u32 - 1);
    let Op::New(c2) = b.lets[j].op else {
        unreachable!()
    };
    b.lets[j].op = Op::Reuse(c2);
    b.lets[j].args.insert(0, Atom::Var(t));
    b.lets.insert(
        from,
        Let {
            binds: vec![t],
            op: Op::DropReuse,
            args: vec![Atom::Var(x)],
            source: None,
        },
    );
}
