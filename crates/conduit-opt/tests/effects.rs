//! Lowers the effects of every test program with `run` checks by evidence
//! passing, then checks that the result verifies, has no effects left, and
//! still passes the `run` checks in the evaluator, and again after the
//! optimizer. After each run, no cycle in the heap may go through the
//! runtime's own objects, which reference counting couldn't free.

use std::collections::BTreeSet;

use conduit_eval::{Machine, ObjId, Object, Value};
use conduit_ir::text;
use conduit_ir::verify::verify_module;
use conduit_ir::{Module, Op};
use conduit_test::{CheckKind, Program};

fn main() {
    conduit_test::main(applies, test)
}

fn applies(p: &Program) -> bool {
    !p.has(|k| matches!(k, CheckKind::VerifyError(_))) && p.has_runs()
}

fn test(p: &Program) -> Result<(), String> {
    let mut m = p.module()?;
    conduit_opt::effects::lower_effects(&mut m)?;
    check(p, &m, "after lowering effects")?;
    conduit_opt::optimize_module(&mut m);
    check(p, &m, "after lowering effects and optimizing")
}

fn check(p: &Program, m: &Module, after: &str) -> Result<(), String> {
    if let Err(errors) = verify_module(m) {
        let errors: Vec<String> = errors.iter().map(|e| e.named(m)).collect();
        return Err(format!(
            "the program {after} doesn't verify:\n{}\n\n{}",
            errors.join("\n"),
            text::print(m)
        ));
    }
    if let Some(left) = effects_left(m) {
        return Err(format!("the program {after} still has {left}"));
    }
    conduit_test::eval::check_runs_then(p, m, |machine| runtime_cycles(m, machine))
        .map_err(|e| format!("{after}: {e}"))
}

/// The data types of the runtime that evidence passing adds.
const RUNTIME: [&str; 4] = ["Yield", "Ev", "Hs", "Node"];

/// An error if a cycle in the heap goes through an object of one of the
/// runtime's data types.
fn runtime_cycles<H: conduit_eval::Host>(
    m: &Module,
    machine: &Machine<'_, Module, H>,
) -> Result<(), String> {
    let objects: Vec<&Object<'_>> = machine.objects().map(|(_, o)| o).collect();
    let edges: Vec<Vec<usize>> = objects
        .iter()
        .map(|o| {
            o.values()
                .iter()
                .filter_map(|&v| match v {
                    Value::Obj(ObjId(i)) => Some(i as usize),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let runtime = |i: usize| match objects[i] {
        Object::Data { ctor, .. } => m.data[&ctor.data]
            .debug_name
            .as_deref()
            .is_some_and(|n| RUNTIME.contains(&n)),
        _ => false,
    };
    for scc in sccs(&edges) {
        let cyclic = scc.len() > 1 || edges[scc[0]].contains(&scc[0]);
        if cyclic && scc.iter().any(|&i| runtime(i)) {
            let kinds: BTreeSet<String> = scc
                .iter()
                .map(|&i| match objects[i] {
                    Object::Data { ctor, .. } => m.data[&ctor.data]
                        .debug_name
                        .as_deref()
                        .map_or_else(|| ctor.data.to_string(), str::to_string),
                    Object::Array { .. } => "array".into(),
                    Object::Ref(_) => "ref".into(),
                    Object::Closure { func, .. } => func.to_string(),
                    Object::Any { .. } => "any".into(),
                    Object::Cont(_) => "cont".into(),
                })
                .collect();
            return Err(format!(
                "a cycle of {} objects goes through the runtime: {}",
                scc.len(),
                kinds.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    Ok(())
}

/// The strongly connected components of a graph, by Tarjan's algorithm,
/// without recursion, since heaps are deep.
fn sccs(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const NONE: usize = usize::MAX;
    let n = edges.len();
    let (mut index, mut low) = (vec![NONE; n], vec![0; n]);
    let mut on_stack = vec![false; n];
    let (mut stack, mut out, mut next) = (Vec::new(), Vec::new(), 0);
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        // Each entry is a node and how many of its edges are done.
        let mut work = vec![(root, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut done)) = work.last_mut() {
            if let Some(&w) = edges[v].get(*done) {
                *done += 1;
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                let mut scc = Vec::new();
                loop {
                    let w = stack.pop().unwrap();
                    on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                out.push(scc);
            }
        }
    }
    out
}

/// What's left of effects in `m`, if anything.
fn effects_left(m: &Module) -> Option<String> {
    if !m.handlers.is_empty() {
        return Some("handlers".into());
    }
    for (id, f) in &m.funcs {
        if !f.row.is_nothing() {
            return Some(format!("a row on {id}"));
        }
        let mut todo = vec![&f.body];
        while let Some(b) = todo.pop() {
            for l in &b.lets {
                if matches!(
                    l.op,
                    Op::Handle(..) | Op::Perform(..) | Op::Resume | Op::Discontinue(..)
                ) {
                    return Some(format!("an effect op in {id}"));
                }
            }
            todo.extend(b.children());
        }
    }
    None
}
