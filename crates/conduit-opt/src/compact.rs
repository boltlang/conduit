//! Renumbers a function's variables and labels after the passes: the
//! ones still bound, in the order the text format prints them, and none
//! of those the passes left behind. Then a name, which the text writes
//! instead of a number where it reads back to the same one, always does.

use std::collections::HashMap;

use conduit_ir::{Atom, Block, Func, Label, Tail, Var};

pub(crate) fn compact(f: &mut Func) {
    let mut order = Order::default();
    for &p in &f.params {
        order.var(p);
    }
    order.block(&f.body);
    let vars: HashMap<Var, Var> = order
        .vars
        .iter()
        .enumerate()
        .map(|(i, &v)| (v, Var(i as u32)))
        .collect();
    let labels: HashMap<Label, Label> = order
        .labels
        .iter()
        .enumerate()
        .map(|(i, &l)| (l, Label(i as u32 + 1)))
        .chain([(Label::RET, Label::RET)])
        .collect();
    f.vars = order
        .vars
        .iter()
        .map(|v| f.vars[v.0 as usize].clone())
        .collect();
    f.labels = order.labels.len() as u32 + 1;
    f.params = f.params.iter().map(|v| vars[v]).collect();
    rename(&mut f.body, &vars, &labels);
    f.debug.vars = std::mem::take(&mut f.debug.vars)
        .into_iter()
        .filter_map(|(v, name)| Some((*vars.get(&v)?, name)))
        .collect();
    f.debug.labels = std::mem::take(&mut f.debug.labels)
        .into_iter()
        .filter_map(|(l, name)| Some((*labels.get(&l)?, name)))
        .collect();
}

/// The variables and labels in the order the text format prints them: a
/// join point's definitions before its body.
#[derive(Default)]
struct Order {
    vars: Vec<Var>,
    labels: Vec<Label>,
    seen: HashMap<Var, ()>,
}

impl Order {
    fn var(&mut self, v: Var) {
        if self.seen.insert(v, ()).is_none() {
            self.vars.push(v);
        }
    }

    fn atoms(&mut self, atoms: &[Atom]) {
        for a in atoms {
            if let Atom::Var(v) = a {
                self.var(*v);
            }
        }
    }

    fn block(&mut self, b: &Block) {
        for l in &b.lets {
            for &v in &l.binds {
                self.var(v);
            }
            self.atoms(&l.args);
        }
        match &b.tail {
            Tail::Join { defs, body, .. } => {
                for d in defs {
                    self.labels.push(d.label);
                    for &v in &d.params {
                        self.var(v);
                    }
                    self.block(&d.body);
                }
                self.block(body);
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                self.atoms(std::slice::from_ref(scrut));
                for a in arms {
                    self.block(&a.body);
                }
                if let Some(d) = default {
                    self.block(d);
                }
            }
            Tail::Jump { args, .. } => self.atoms(args),
            Tail::Unreachable | Tail::Trap => {}
        }
    }
}

fn rename(b: &mut Block, vars: &HashMap<Var, Var>, labels: &HashMap<Label, Label>) {
    let atoms = |atoms: &mut [Atom]| {
        for a in atoms {
            if let Atom::Var(v) = a {
                *v = vars[v];
            }
        }
    };
    for l in &mut b.lets {
        for v in &mut l.binds {
            *v = vars[v];
        }
        atoms(&mut l.args);
    }
    match &mut b.tail {
        Tail::Join { defs, body, .. } => {
            for d in defs {
                d.label = labels[&d.label];
                for v in &mut d.params {
                    *v = vars[v];
                }
                rename(&mut d.body, vars, labels);
            }
            rename(body, vars, labels);
        }
        Tail::Case {
            scrut,
            arms,
            default,
        } => {
            atoms(std::slice::from_mut(scrut));
            for a in arms {
                rename(&mut a.body, vars, labels);
            }
            if let Some(d) = default {
                rename(d, vars, labels);
            }
        }
        Tail::Jump { target, args, .. } => {
            *target = labels.get(target).copied().unwrap_or(*target);
            atoms(args);
        }
        Tail::Unreachable | Tail::Trap => {}
    }
}
