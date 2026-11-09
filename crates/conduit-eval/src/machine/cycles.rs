//! Collecting cycles by trial deletion (Bacon and Rajan, synchronous): see
//! [`Machine::cycles`].

use super::*;

impl<'p, P: Program + ?Sized, H: Host> Machine<'p, P, H> {
    /// Buffers an object that a `drop` left above 0, if it may be the root
    /// of a garbage cycle, and collects once the buffer is full.
    pub(super) fn possible_root(&mut self, id: ObjId) {
        let i = id.0 as usize;
        if !self.cycles || !may_cycle(&self.heap[i]) || self.colors[i] == Color::Purple {
            return;
        }
        self.colors[i] = Color::Purple;
        if !self.buffered[i] {
            self.buffered[i] = true;
            self.roots.push(id);
            if self.roots.len() >= self.cycle_buffer {
                self.collect_cycles();
            }
        }
    }

    /// The counted objects `id` holds, once per reference.
    pub(super) fn children(&self, id: ObjId) -> Vec<ObjId> {
        (self.heap[id.0 as usize].values().iter())
            .filter_map(as_obj)
            .filter(|t| matches!(self.counts.get(t.0 as usize), Some(Count::Live(_))))
            .collect()
    }

    pub(super) fn count_mut(&mut self, id: ObjId) -> &mut u32 {
        match &mut self.counts[id.0 as usize] {
            Count::Live(c) => c,
            _ => unreachable!("only live objects are traversed"),
        }
    }

    /// Frees the garbage cycles among the objects buffered as possible
    /// roots, and empties the buffer.
    pub fn collect_cycles(&mut self) {
        let roots = std::mem::take(&mut self.roots);
        if !roots.is_empty() {
            self.stats.collections += 1;
        }
        // Mark gray from each root that's still purple, taking away the
        // counts its subgraph gives itself.
        let mut kept = Vec::new();
        for s in roots {
            let i = s.0 as usize;
            if self.colors[i] == Color::Purple && matches!(self.counts[i], Count::Live(_)) {
                self.mark_gray(s);
                kept.push(s);
            } else {
                self.buffered[i] = false;
            }
        }
        // What's still above 0 is held from outside: it and what it reaches
        // are black again, with their counts back. The rest is white.
        for &s in &kept {
            self.scan(s);
        }
        // White is garbage that only cycles hold.
        for s in kept {
            self.buffered[s.0 as usize] = false;
            self.collect_white(s);
        }
    }

    pub(super) fn mark_gray(&mut self, s: ObjId) {
        let mut todo = vec![s];
        while let Some(u) = todo.pop() {
            if self.colors[u.0 as usize] == Color::Gray {
                continue;
            }
            self.colors[u.0 as usize] = Color::Gray;
            for t in self.children(u) {
                *self.count_mut(t) -= 1;
                todo.push(t);
            }
        }
    }

    pub(super) fn scan(&mut self, s: ObjId) {
        let mut todo = vec![s];
        while let Some(u) = todo.pop() {
            if self.colors[u.0 as usize] != Color::Gray {
                continue;
            }
            if *self.count_mut(u) > 0 {
                self.scan_black(u);
            } else {
                self.colors[u.0 as usize] = Color::White;
                todo.extend(self.children(u));
            }
        }
    }

    pub(super) fn scan_black(&mut self, s: ObjId) {
        self.colors[s.0 as usize] = Color::Black;
        let mut todo = vec![s];
        while let Some(u) = todo.pop() {
            for t in self.children(u) {
                *self.count_mut(t) += 1;
                if self.colors[t.0 as usize] != Color::Black {
                    self.colors[t.0 as usize] = Color::Black;
                    todo.push(t);
                }
            }
        }
    }

    pub(super) fn collect_white(&mut self, s: ObjId) {
        let mut todo = vec![s];
        let mut white = Vec::new();
        while let Some(u) = todo.pop() {
            let i = u.0 as usize;
            if self.colors[i] != Color::White || self.buffered[i] {
                continue;
            }
            self.colors[i] = Color::Black;
            todo.extend(self.children(u));
            white.push(u);
        }
        // Freed only once found, so that the traversal sees them live.
        for u in white {
            self.counts[u.0 as usize] = Count::Freed;
            self.freed();
        }
    }
}

/// The colors of trial deletion: black is in use or not yet looked at,
/// gray is being looked at, white is garbage, and purple is a possible
/// root of a garbage cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Color {
    Black,
    Gray,
    White,
    Purple,
}

/// Whether an object may be in a cycle: an `Imm` one can't, since it holds
/// only `Imm` objects, made before it. A continuation's frames can't be
/// looked into, so a continuation isn't either.
pub(super) fn may_cycle(o: &Object) -> bool {
    match o {
        Object::Data { is_mut, .. }
        | Object::Array { is_mut, .. }
        | Object::Closure { is_mut, .. } => *is_mut,
        Object::Ref(_) | Object::Any { .. } => true,
        Object::Cont(_) | Object::Host(_) | Object::HostFunction(_) => false,
    }
}
