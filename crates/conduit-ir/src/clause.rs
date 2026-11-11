//! Handler clauses that a backend can compile without capturing a
//! continuation. See "Two kinds of clause a backend should notice" in
//! `docs/ir.md`.

use crate::{Atom, Block, Func, HandlerId, Label, Module, Op, Tail, Ty, Var};

/// The clause `f` as a plain function, if it's **tail-resumptive**: every
/// way out of it is `let rs = resume(k, xs); jump ret(rs)` or
/// `let rs = discontinue E.i(k, xs); jump ret(rs)`, and it uses its
/// continuation `k`, its last parameter, nowhere else.
///
/// Such a clause resumes once, with `xs`, and then gives back what the rest
/// of the body gave. So a `perform` it handles can be a call to the
/// function this returns, which takes the same parameters but `k`, and
/// gives `xs` instead of resuming: the code after the `perform` then runs as
/// the rest of the body. The clause must run under the handlers outside its
/// own, as it would if `k` were captured.
///
/// A `discontinue` becomes `let ys = discontinue E.i(xs); jump ret(ys)`,
/// without `k`, with `ys` fresh variables of the types `k` takes. It means
/// `perform E.i(xs)`, but under the handlers of the `perform` the clause
/// handles, as it would run if `k` were captured. Only a backend sees it:
/// the verifier doesn't accept a `discontinue` without a continuation.
///
/// `k`'s variable is kept, unbound and of type `i32`, so that no other
/// variable changes number.
pub fn plain_call(f: &Func) -> Option<Func> {
    let &k = f.params.last()?;
    let Ty::Cont(sig) = f.var_ty(k) else {
        return None;
    };
    let mut plain = f.clone();
    strip_counts(&mut plain.body, k);
    plain.params.pop();
    plain.results = sig.params.clone();
    plain.vars[k.0 as usize] = Ty::I32;
    let mut cx = Rewrite {
        k,
        takes: &sig.params,
        vars: &mut plain.vars,
    };
    cx.block(&mut plain.body).then_some(plain)
}

/// Whether every clause of `h` is tail-resumptive or aborting, so that a
/// backend can lower the handler without capturing a continuation.
pub fn direct_handler(m: &Module, h: HandlerId) -> bool {
    m.handlers.get(&h).is_some_and(|decl| {
        decl.clauses().all(|(_, _, c)| {
            m.funcs
                .get(&c)
                .is_some_and(|f| plain_call(f).is_some() || aborting(f).is_some())
        })
    })
}

/// Whether every handler of `m` is [direct](direct_handler). One that
/// isn't needs real continuations: stack switching, or evidence passing.
pub fn direct_handlers(m: &Module) -> bool {
    m.handlers.keys().all(|&h| direct_handler(m, h))
}

/// Whether some way out of `f`, a plain function from [`plain_call`],
/// discontinues.
pub fn discontinues(f: &Func) -> bool {
    f.body
        .all_lets()
        .any(|l| matches!(l.op, Op::Discontinue(..)))
}

/// The clause `f` without its continuation `k`, its last parameter, if
/// it's **aborting**: it never uses `k`.
///
/// What such a clause gives is what the whole `handle` gives, without
/// running the rest of the body or the return clause. So a `perform` it
/// handles can call the function this returns, which takes the same
/// parameters but `k`, and then unwind to the `handle` with its results.
/// As with [`plain_call`], the clause must run under the handlers outside
/// its own, and `k`'s variable is kept, unbound and of type `i32`.
pub fn aborting(f: &Func) -> Option<Func> {
    let &k = f.params.last()?;
    if !matches!(f.var_ty(k), Ty::Cont(_)) {
        return None;
    }
    let mut plain = f.clone();
    strip_counts(&mut plain.body, k);
    if uses(&plain.body, k) {
        return None;
    }
    plain.params.pop();
    plain.vars[k.0 as usize] = Ty::I32;
    Some(plain)
}

/// `b` without the ops that count `k`'s references: without `k`, there's
/// nothing to count, and what it held is lost, as when a continuation is
/// dropped.
fn strip_counts(b: &mut Block, k: Var) {
    b.for_each_mut(|b| {
        b.lets.retain(|l| {
            // Those that bind something are never of a continuation.
            !(l.op.is_count() && l.binds.is_empty() && l.args == [Atom::Var(k)])
        })
    });
}

/// Whether `b` uses `v` anywhere.
fn uses(b: &Block, v: Var) -> bool {
    let v = Atom::Var(v);
    b.blocks().any(|b| b.atoms().any(|a| *a == v))
}

struct Rewrite<'a> {
    k: Var,
    /// The types `k` takes, which a `perform` gives.
    takes: &'a [Ty],
    vars: &'a mut Vec<Ty>,
}

impl Rewrite<'_> {
    /// Rewrites each `let rs = resume(k, xs); jump ret(rs)` in `b` to
    /// `jump ret(xs)`, and each `let rs = discontinue E.i(k, xs);
    /// jump ret(rs)` to `let ys = discontinue E.i(xs); jump ret(ys)`. Gives
    /// `false` if `k` is used any other way, or a way out doesn't resume or
    /// discontinue.
    fn block(&mut self, b: &mut Block) -> bool {
        crate::deep(|| {
            let k = self.k;
            let uses = |args: &[Atom]| args.contains(&Atom::Var(k));
            match &mut b.tail {
                Tail::Jump { target, args, .. } if *target == Label::RET => {
                    let Some(last) = b.lets.last_mut() else {
                        return false;
                    };
                    let passes_on = args.len() == last.binds.len()
                        && args
                            .iter()
                            .zip(&last.binds)
                            .all(|(a, &r)| *a == Atom::Var(r));
                    let ends = matches!(last.op, Op::Resume | Op::Discontinue(..))
                        && last.args.first() == Some(&Atom::Var(k))
                        && passes_on;
                    if !ends || uses(&last.args[1..]) {
                        return false;
                    }
                    if last.op == Op::Resume {
                        *args = b.lets.pop().unwrap().args.split_off(1);
                    } else {
                        last.args.remove(0);
                        last.binds = self
                            .takes
                            .iter()
                            .map(|ty| {
                                self.vars.push(ty.clone());
                                Var(self.vars.len() as u32 - 1)
                            })
                            .collect();
                        *args = last.binds.iter().map(|&v| Atom::Var(v)).collect();
                    }
                }
                Tail::Jump { args, .. } => {
                    if uses(args) {
                        return false;
                    }
                }
                Tail::Join { defs, body, .. } => {
                    if !defs.iter_mut().all(|d| self.block(&mut d.body)) || !self.block(body) {
                        return false;
                    }
                }
                Tail::Case {
                    scrut,
                    arms,
                    default,
                } => {
                    if *scrut == Atom::Var(k)
                        || !arms.iter_mut().all(|a| self.block(&mut a.body))
                        || !default.as_deref_mut().is_none_or(|d| self.block(d))
                    {
                        return false;
                    }
                }
                Tail::Unreachable | Tail::Trap => {}
            }
            // The `resume` or `discontinue` before a `jump ret` has no `k` by
            // now.
            !b.lets.iter().any(|l| uses(&l.args))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{parse, print_func};
    use crate::verify::verify_func;
    use crate::{FuncId, Module};

    fn plain(src: &str) -> Option<String> {
        let mut m: Module = parse(src).unwrap();
        let id = FuncId(1);
        let f = plain_call(&m.funcs[&id])?;
        m.funcs.insert(id, f.clone());
        verify_func(&m, id, &f).unwrap();
        Some(print_func(id, &f))
    }

    #[test]
    fn rewrites_tail_resumes() {
        // Resumes on both arms, after reading and writing its environment.
        let src = "fn fn1(%0: ref i32, %1: i32, %2: cont(i32) -> (i64)) -> (i64) {
                     let %3: i32 = ref_get(%0)
                     case %1 {
                       0 =>
                         let %4: i64 = resume(%2, %3)
                         jump ret(%4)
                       _ =>
                         let = ref_set(%0, %1)
                         let %5: i64 = resume(%2, %1)
                         jump ret(%5)
                     }
                   }";
        let got = plain(src).unwrap();
        assert!(
            got.starts_with("fn fn1(%0: ref i32, %1: i32) -> (i32)"),
            "{got}"
        );
        assert!(got.contains("jump ret(%3)") && got.contains("jump ret(%1)"));
        assert!(!got.contains("resume"));
    }

    #[test]
    fn rewrites_tail_discontinues() {
        let src = "effect effect20 { (i32) -> (i32) }
                   fn fn1(%0: i32, %1: cont(i32) -> (i64)) -> (i64) {
                     case %0 {
                       0 =>
                         let %2: i64 = resume(%1, %0)
                         jump ret(%2)
                       _ =>
                         let %3: i64 = discontinue effect20.0(%1, %0)
                         jump ret(%3)
                     }
                   }";
        let m: Module = parse(src).unwrap();
        let f = plain_call(&m.funcs[&FuncId(1)]).unwrap();
        assert!(discontinues(&f));
        let got = print_func(FuncId(1), &f);
        assert!(got.starts_with("fn fn1(%0: i32) -> (i32)"), "{got}");
        // A fresh variable of the type `k` takes, without `k`.
        assert!(
            got.contains("let %4: i32 = discontinue effect20.0(%0)")
                && got.contains("jump ret(%4)"),
            "{got}"
        );
        // Not in tail position.
        let src = "effect effect20 { (i32) -> (i32) }
                   fn fn1(%0: i32, %1: cont(i32) -> (i64)) -> (i64) {
                     let %2: i64 = discontinue effect20.0(%1, %0)
                     let %3: i64 = add(%2, 1i64)
                     jump ret(%3)
                   }";
        let m: Module = parse(src).unwrap();
        assert!(plain_call(&m.funcs[&FuncId(1)]).is_none());
    }

    #[test]
    fn finds_aborting_clauses() {
        let clause = |body: &str| {
            let src = format!("fn fn1(%0: i32, %1: cont(i32) -> (i64)) -> (i64) {{ {body} }}");
            let mut m: Module = parse(&src).unwrap();
            let f = aborting(&m.funcs[&FuncId(1)])?;
            m.funcs.insert(FuncId(1), f.clone());
            verify_func(&m, FuncId(1), &f).unwrap();
            Some(print_func(FuncId(1), &f))
        };
        let got = clause(
            "let %2: i64 = convert.s[i64](%0)
             jump ret(%2)",
        )
        .unwrap();
        assert!(got.starts_with("fn fn1(%0: i32) -> (i64)"), "{got}");
        assert_eq!(
            clause(
                "let %2: i64 = resume(%1, %0)
                 jump ret(%2)"
            ),
            None
        );
        // Uses `k` on one path only: neither kind.
        assert_eq!(
            clause(
                "case %0 {
                   0 =>
                     let %2: i64 = resume(%1, %0)
                     jump ret(%2)
                   _ => jump ret(0i64)
                 }"
            ),
            None
        );
    }

    #[test]
    fn keeps_others() {
        for body in [
            // Does something after resuming.
            "let %3: i32 = resume(%2, %1)
             let %4: i32 = add(%3, 1i32)
             jump ret(%4)",
            // Never resumes: aborting.
            "jump ret(%1)",
            // Resumes on one path only.
            "join j1() = jump ret(%1)
             in case %1 {
               0 => jump j1()
               _ =>
                 let %3: i32 = resume(%2, %1)
                 jump ret(%3)
             }",
            // Passes `k` on.
            "let %3: i32 = call fn2(%2)
             jump ret(%3)",
        ] {
            let src = format!(
                "fn fn1(%0: ref i32, %1: i32, %2: cont(i32) -> (i32)) -> (i32) {{ {body} }}
                 fn fn2(%0: cont(i32) -> (i32)) -> (i32) {{ unreachable }}"
            );
            assert_eq!(plain(&src), None, "{body}");
        }
    }
}
