//! Handler clauses that a backend can compile without capturing a
//! continuation. See "Two kinds of clause a backend should notice" in
//! `docs/ir.md`.

use crate::{Atom, Block, Func, Label, Op, Tail, Ty, Var};

/// The clause `f` as a plain function, if it's **tail-resumptive**: every
/// way out of it is `let rs = resume(k, xs); jump ret(rs)`, and it uses its
/// continuation `k`, its last parameter, nowhere else.
///
/// Such a clause resumes once, with `xs`, and then gives back what the rest
/// of the body gave. So a `perform` it handles can be a call to the
/// function this returns, which takes the same parameters but `k`, and
/// gives `xs` instead of resuming: the code after the `perform` then runs as
/// the rest of the body. The clause must run under the handlers outside its
/// own, as it would if `k` were captured.
///
/// `k`'s variable is kept, unbound and of type `i32`, so that no other
/// variable changes number.
pub fn plain_call(f: &Func) -> Option<Func> {
    let &k = f.params.last()?;
    let Ty::Cont(sig) = f.var_ty(k) else {
        return None;
    };
    let mut plain = f.clone();
    plain.params.pop();
    plain.results = sig.params.clone();
    plain.vars[k.0 as usize] = Ty::I32;
    rewrite(&mut plain.body, k).then_some(plain)
}

/// Rewrites each `let rs = resume(k, xs); jump ret(rs)` in `b` to
/// `jump ret(xs)`. Gives `false` if `k` is used any other way, or a way out
/// doesn't resume.
fn rewrite(b: &mut Block, k: Var) -> bool {
    let uses = |args: &[Atom]| args.contains(&Atom::Var(k));
    match &mut b.tail {
        Tail::Jump { target, args, .. } if *target == Label::RET => {
            let Some(last) = b.lets.last() else {
                return false;
            };
            let resumes = last.op == Op::Resume
                && last.args.first() == Some(&Atom::Var(k))
                && args.len() == last.binds.len()
                && args
                    .iter()
                    .zip(&last.binds)
                    .all(|(a, &r)| *a == Atom::Var(r));
            if !resumes || uses(&last.args[1..]) {
                return false;
            }
            *args = b.lets.pop().unwrap().args.split_off(1);
        }
        Tail::Jump { args, .. } => {
            if uses(args) {
                return false;
            }
        }
        Tail::Join { defs, body, .. } => {
            if !defs.iter_mut().all(|d| rewrite(&mut d.body, k)) || !rewrite(body, k) {
                return false;
            }
        }
        Tail::Case {
            scrut,
            arms,
            default,
        } => {
            if *scrut == Atom::Var(k)
                || !arms.iter_mut().all(|a| rewrite(&mut a.body, k))
                || !default.as_deref_mut().is_none_or(|d| rewrite(d, k))
            {
                return false;
            }
        }
        Tail::Unreachable => {}
    }
    // The `resume` before a `jump ret` is gone by now.
    !b.lets.iter().any(|l| uses(&l.args))
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
