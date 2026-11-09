//! Reference counting: `dup`, `drop` and the ops that count for
//! themselves change counts and free, and [`Machine::leaks`] says what a run
//! left behind.

use super::*;

/// The most a reference count can be: a `dup` past it makes the object
/// immortal instead, so that it leaks rather than being freed while it's
/// still in use. It's what fits in the 28 bits of a Wasm object's header,
/// so that the evaluator and Wasm free the same objects.
pub const MAX_COUNT: u32 = 0x0fff_ffff;

impl<'p, P: Program + ?Sized, H: Host> Machine<'p, P, H> {
    /// An object's reference count.
    pub fn count(&self, id: ObjId) -> Option<Count> {
        self.counts.get(id.0 as usize).copied()
    }

    /// What a counted run left behind: objects whose counts don't match the
    /// references to them, references to freed objects, and garbage that
    /// only cycles keep alive. `roots` are the values that still own an
    /// object after the run, like its results. Only meaningful with
    /// [`counting`](Self::counting) on, and once the machine has stopped.
    ///
    /// The values in a continuation's frames aren't counted as references,
    /// since a frame keeps values it has already given away. So what a
    /// continuation that's never resumed holds shows up as lost, apart from
    /// what's undropped.
    pub fn leaks(&self, roots: &[Value]) -> Leaks {
        let n = self.heap.len();
        let live = |id: usize| matches!(self.counts[id], Count::Live(_));
        let mut refs = vec![0u32; n];
        let mut leaks = Leaks::default();
        let point = |v: &Value, refs: &mut Vec<u32>, leaks: &mut Leaks| {
            if let Value::Obj(t) = v {
                match self.counts.get(t.0 as usize) {
                    Some(Count::Freed) => leaks.dangling.push(*t),
                    Some(_) => refs[t.0 as usize] += 1,
                    None => {}
                }
            }
        };
        // The closures of host functions the host holds are its, as roots.
        for v in roots.iter().chain(self.held_functions()) {
            point(v, &mut refs, &mut leaks);
        }
        for id in (0..n).filter(|&id| matches!(self.counts[id], Count::Live(_) | Count::Immortal)) {
            for v in self.heap[id].values() {
                point(v, &mut refs, &mut leaks);
            }
        }
        for id in 0..n {
            // A token never used is memory never given back.
            if self.counts[id] == Count::Reserved {
                leaks.undropped.push((ObjId(id as u32), 1));
            }
            if let Count::Live(c) = self.counts[id] {
                let o = ObjId(id as u32);
                if c > refs[id] {
                    leaks.undropped.push((o, c - refs[id]));
                } else if c < refs[id] {
                    leaks.overdropped.push((o, refs[id] - c));
                }
            }
        }
        // An undropped object that the frames of a continuation that was
        // never resumed can reach was lost with it.
        let mut in_frames = vec![false; n];
        let mut todo: Vec<ObjId> = Vec::new();
        for object in &self.heap {
            if let Object::Cont(Continuation {
                frames: Some(frames),
            }) = object
            {
                for frame in frames {
                    match frame {
                        Frame::Code(code) => {
                            todo.extend(code.vars.iter().flatten().filter_map(as_obj))
                        }
                        Frame::Handler { env, .. } => todo.extend(env.iter().filter_map(as_obj)),
                    }
                }
            }
        }
        while let Some(o) = todo.pop() {
            let id = o.0 as usize;
            if id >= n || in_frames[id] || !live(id) {
                continue;
            }
            in_frames[id] = true;
            todo.extend(self.heap[id].values().iter().filter_map(as_obj));
        }
        let (lost, undropped) = leaks
            .undropped
            .iter()
            .partition(|(o, _)| in_frames[o.0 as usize]);
        leaks.lost = lost;
        leaks.undropped = undropped;
        // What the roots reach is in use, and what an undropped or lost
        // object reaches is lost through it. What's left, if live, only
        // garbage holds, so cycles keep it alive.
        let mut seen = vec![false; n];
        todo = roots
            .iter()
            .chain(self.held_functions())
            .filter_map(as_obj)
            .collect();
        todo.extend(leaks.undropped.iter().chain(&leaks.lost).map(|(o, _)| *o));
        while let Some(o) = todo.pop() {
            let id = o.0 as usize;
            if id >= n || seen[id] || !live(id) {
                continue;
            }
            seen[id] = true;
            todo.extend(self.heap[id].values().iter().filter_map(as_obj));
        }
        leaks.cycles = (0..n)
            .filter(|&id| live(id) && !seen[id])
            .map(|id| ObjId(id as u32))
            .collect();
        leaks.dangling.sort();
        leaks.dangling.dedup();
        leaks
    }

    /// Adds `n` to the count of `value`'s object, if it has one.
    pub(super) fn dup_n(&mut self, value: Value, n: u32) -> Step<()> {
        let Value::Obj(id) = value else {
            return Ok(());
        };
        if !self.counting || n == 0 {
            return Ok(());
        }
        if let Some(color) = self.colors.get_mut(id.0 as usize) {
            *color = Color::Black;
        }
        match self.counts.get_mut(id.0 as usize) {
            Some(count @ Count::Live(_)) => {
                let Count::Live(c) = *count else {
                    unreachable!()
                };
                // Past the most a count can be, it saturates: the object is
                // never freed, as in Wasm, whose header has 28 bits for it.
                *count = match c.checked_add(n) {
                    Some(sum) if sum <= MAX_COUNT => Count::Live(sum),
                    _ => Count::Immortal,
                };
                Ok(())
            }
            Some(Count::Immortal) => Ok(()),
            Some(Count::Freed | Count::Reserved) => Err(freed("dup")),
            None => Err(unverified("dangling object")),
        }
    }

    pub(super) fn dup(&mut self, value: Value) -> Step<()> {
        self.dup_n(value, 1)
    }

    /// Takes 1 from the count of `value`'s object, if it has one, and frees
    /// it at 0, which drops what it holds, without recursion. A
    /// continuation's frames aren't dropped: what they hold is lost.
    pub(super) fn drop_value(&mut self, value: Value) -> Step<()> {
        if !self.counting {
            return Ok(());
        }
        let mut todo = vec![value];
        while let Some(value) = todo.pop() {
            let Value::Obj(id) = value else { continue };
            let i = id.0 as usize;
            match self.counts.get_mut(i) {
                Some(Count::Live(1)) => {
                    self.counts[i] = Count::Freed;
                    self.freed();
                    todo.extend_from_slice(self.heap[i].values());
                }
                Some(Count::Live(c)) => {
                    *c -= 1;
                    self.possible_root(id);
                }
                Some(Count::Immortal) => {}
                Some(Count::Freed | Count::Reserved) => return Err(freed("drop")),
                None => return Err(unverified("dangling object")),
            }
        }
        Ok(())
    }

    /// `drop_reuse`: like `drop`, but at 0 the memory is kept for the token
    /// it gives, which is otherwise none, `Unset`. Without counting, it's
    /// always none, and nothing changes.
    pub(super) fn drop_reuse(&mut self, value: Value) -> Step<Value> {
        let id = obj(value)?;
        if !self.counting {
            return Ok(Value::Unset);
        }
        let i = id.0 as usize;
        match self.counts.get(i) {
            Some(Count::Live(1)) => {
                self.counts[i] = Count::Reserved;
                let held = self.heap[i].values().to_vec();
                for v in held {
                    self.drop_value(v)?;
                }
                Ok(Value::Obj(id))
            }
            Some(Count::Live(_) | Count::Immortal) => {
                self.drop_value(value)?;
                Ok(Value::Unset)
            }
            Some(Count::Freed | Count::Reserved) => Err(freed("drop_reuse")),
            None => Err(unverified("dangling object")),
        }
    }

    /// Whether `value`'s object has a count of 1. Without counting, none
    /// does, and neither does an uncounted object.
    pub(super) fn is_unique(&self, value: Value) -> Step<bool> {
        let id = obj(value)?;
        match self.counts.get(id.0 as usize) {
            Some(Count::Live(1)) => Ok(self.counting),
            Some(Count::Live(_) | Count::Immortal) => Ok(false),
            Some(Count::Freed | Count::Reserved) => Err(freed("is_unique")),
            None => Err(unverified("dangling object")),
        }
    }

    /// Frees an object whose count is 1, without dropping what it holds.
    pub(super) fn free(&mut self, value: Value) -> Step<()> {
        let id = obj(value)?;
        if !self.counting {
            return Ok(());
        }
        match self.counts.get_mut(id.0 as usize) {
            Some(c @ Count::Live(1)) => {
                *c = Count::Freed;
                self.freed();
            }
            Some(Count::Freed | Count::Reserved) => return Err(freed("free")),
            Some(_) => return Err(ErrorKind::Undefined("free of a shared object".into())),
            None => return Err(unverified("dangling object")),
        }
        Ok(())
    }

    /// Takes 1 from the count of an object that's shared.
    pub(super) fn decref(&mut self, value: Value) -> Step<()> {
        let id = obj(value)?;
        if !self.counting {
            return Ok(());
        }
        match self.counts.get_mut(id.0 as usize) {
            Some(Count::Live(1)) => {
                return Err(ErrorKind::Undefined("decref of a unique object".into()));
            }
            Some(Count::Live(c)) => {
                *c -= 1;
                self.possible_root(id);
            }
            Some(Count::Immortal) => {}
            Some(Count::Freed | Count::Reserved) => return Err(freed("decref")),
            None => return Err(unverified("dangling object")),
        }
        Ok(())
    }

    /// Counts an object freed.
    pub(super) fn freed(&mut self) {
        self.stats.frees += 1;
        self.live -= 1;
    }

    pub(super) fn dup_all(&mut self, values: &[Value]) -> Step<()> {
        values.iter().try_for_each(|v| self.dup(*v))
    }
}

/// An object's reference count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// How many references own the object. It starts at 1.
    Live(u32),
    /// Its count reached 0. Without [`counting`](crate::Machine::counting),
    /// nothing is freed.
    Freed,
    /// Never counted: a constant's object.
    Immortal,
    /// Freed by `drop_reuse`, its memory held by a reuse token until a
    /// `reuse` builds a new object in it.
    Reserved,
}

/// What [`Machine::leaks`](crate::Machine::leaks) found. Each list is
/// sorted by object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leaks {
    /// Objects with more counts than references, and how many more:
    /// something that owned a reference never dropped it.
    pub undropped: Vec<(ObjId, u32)>,
    /// Objects with fewer counts than references, and how many fewer:
    /// something dropped a reference it didn't own.
    pub overdropped: Vec<(ObjId, u32)>,
    /// Undropped objects that the frames of a continuation that was never
    /// resumed can reach: dropping a continuation loses what its frames
    /// hold, which is expected until a dropped continuation is unwound.
    pub lost: Vec<(ObjId, u32)>,
    /// Freed objects that a live object or a root still refers to.
    pub dangling: Vec<ObjId>,
    /// Garbage whose counts are all right: only other garbage refers to
    /// it, so it's in a cycle, or reachable only from one. Counting can't
    /// free it, which is expected until there is a cycle collector.
    pub cycles: Vec<ObjId>,
}

impl Leaks {
    /// Whether every count is right and nothing is left but cycles.
    pub fn counts_ok(&self) -> bool {
        self.undropped.is_empty() && self.overdropped.is_empty() && self.dangling.is_empty()
    }

    /// Whether everything the run made that the roots don't own was freed.
    pub fn is_empty(&self) -> bool {
        self.counts_ok() && self.cycles.is_empty() && self.lost.is_empty()
    }
}

impl std::fmt::Display for Leaks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let objs = |list: &mut dyn Iterator<Item = String>| list.collect::<Vec<_>>().join(", ");
        let mut parts = Vec::new();
        if !self.undropped.is_empty() {
            let list = objs(
                &mut self
                    .undropped
                    .iter()
                    .map(|(o, n)| format!("#{} (+{n})", o.0)),
            );
            parts.push(format!("undropped: {list}"));
        }
        if !self.overdropped.is_empty() {
            let list = objs(
                &mut self
                    .overdropped
                    .iter()
                    .map(|(o, n)| format!("#{} (-{n})", o.0)),
            );
            parts.push(format!("dropped too often: {list}"));
        }
        if !self.dangling.is_empty() {
            let list = objs(&mut self.dangling.iter().map(|o| format!("#{}", o.0)));
            parts.push(format!("freed but referred to: {list}"));
        }
        if !self.lost.is_empty() {
            let list = objs(&mut self.lost.iter().map(|(o, n)| format!("#{} (+{n})", o.0)));
            parts.push(format!("lost with continuations: {list}"));
        }
        if !self.cycles.is_empty() {
            let list = objs(&mut self.cycles.iter().map(|o| format!("#{}", o.0)));
            parts.push(format!("kept alive by cycles: {list}"));
        }
        if parts.is_empty() {
            write!(f, "no leaks")
        } else {
            write!(f, "{}", parts.join("; "))
        }
    }
}

/// What a [`Machine`](crate::Machine) has done, for benchmarks: numbers that
/// are the same each time a program runs, unlike its time, so a change in
/// them is a change in the program or in what compiled it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// `let`s and tails run.
    pub steps: u64,
    /// Objects allocated, constants not among them.
    pub allocs: u64,
    /// Objects freed, by counting or by the cycle collector.
    pub frees: u64,
    /// The most objects allocated and not freed at once.
    pub peak_live: u64,
    /// Ops that only count references, run: `dup`, `drop`, `is_unique`,
    /// `free`, `decref`, `drop_reuse` and `reuse_token`.
    pub count_ops: u64,
    /// `reuse`s that built in a token's memory, and those that allocated.
    pub reused: u64,
    pub fresh: u64,
    /// Cycle collections that had something to look at.
    pub collections: u64,
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "steps {}, allocs {}, frees {}, peak live {}, count ops {}, reused {} of {}, collections {}",
            self.steps,
            self.allocs,
            self.frees,
            self.peak_live,
            self.count_ops,
            self.reused,
            self.reused + self.fresh,
            self.collections
        )
    }
}
