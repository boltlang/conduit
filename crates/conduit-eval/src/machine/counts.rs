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
    /// continuation that's never resumed nor dropped holds shows up as
    /// lost, apart from what's undropped.
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
        let in_frames = self.reach(todo);
        let (lost, undropped) = leaks
            .undropped
            .iter()
            .partition(|(o, _)| in_frames[o.0 as usize]);
        leaks.lost = lost;
        leaks.undropped = undropped;
        // What the roots reach is in use, and what an undropped or lost
        // object reaches is lost through it. What's left, if live, only
        // garbage holds, so cycles keep it alive.
        let mut todo: Vec<ObjId> = (roots.iter().chain(self.held_functions()))
            .filter_map(as_obj)
            .collect();
        todo.extend(leaks.undropped.iter().chain(&leaks.lost).map(|(o, _)| *o));
        let seen = self.reach(todo);
        leaks.cycles = (0..n)
            .filter(|&id| live(id) && !seen[id])
            .map(|id| ObjId(id as u32))
            .collect();
        leaks.dangling.sort();
        leaks.dangling.dedup();
        leaks
    }

    /// Which live objects `from` reaches, by object: those in it and what
    /// they hold, through live objects only.
    fn reach(&self, mut from: Vec<ObjId>) -> Vec<bool> {
        let mut seen = vec![false; self.heap.len()];
        while let Some(o) = from.pop() {
            let id = o.0 as usize;
            if id >= seen.len() || seen[id] || !matches!(self.counts[id], Count::Live(_)) {
                continue;
            }
            seen[id] = true;
            from.extend(self.heap[id].values().iter().filter_map(as_obj));
        }
        seen
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
        let Some(count) = self.counts.get_mut(id.0 as usize) else {
            return Err(unverified("dangling object"));
        };
        match *count {
            // Past the most a count can be, it saturates: the object is
            // never freed, as in Wasm, whose header has 28 bits for it.
            Count::Live(c) => {
                *count = match c.checked_add(n) {
                    Some(sum) if sum <= MAX_COUNT => Count::Live(sum),
                    _ => Count::Immortal,
                };
                Ok(())
            }
            Count::Immortal => Ok(()),
            Count::Freed | Count::Reserved => Err(freed("dup")),
        }
    }

    pub(super) fn dup(&mut self, value: Value) -> Step<()> {
        self.dup_n(value, 1)
    }

    /// Takes 1 from the count of `value`'s object, if it has one, and frees
    /// it at 0, which drops what it holds, without recursion. A
    /// continuation that was never resumed holds what its frames own (see
    /// [`Self::frames_own`]).
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
                    self.no_undefined(i)?;
                    todo.extend_from_slice(self.heap[i].values());
                    if let Object::Cont(cont) = &mut self.heap[i]
                        && let Some(frames) = cont.frames.take()
                    {
                        todo.extend(self.frames_own(&frames)?);
                    }
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

    /// What `frames`, of a continuation that's never resumed, own, a value
    /// for each reference: what each running function's code would have
    /// given away or dropped after the `let` it waits on (see
    /// [`conduit_ir::owned`]), and each handler's environment, which its
    /// return clause would have taken.
    fn frames_own(&self, frames: &[Frame<'p>]) -> Step<Vec<Value>> {
        let p = self.p;
        let mut out = Vec::new();
        for frame in frames {
            match frame {
                Frame::Handler { env, .. } => out.extend_from_slice(env),
                Frame::Code(code) => {
                    let Some((_, next)) = code.waiting else {
                        return Err(unverified("a continuation's frame that doesn't wait"));
                    };
                    let f = (p.func(code.func))
                        .ok_or_else(|| unverified(format!("unknown function {}", code.func)))?;
                    let owned = conduit_ir::owned::Owned::new(f, |g, i| {
                        p.func(g).is_some_and(|g| g.is_borrowed(i))
                    });
                    for (v, n) in owned.during(next.block, next.index - 1) {
                        let value = code.vars.get(v.0 as usize).copied().flatten();
                        let value = value.ok_or_else(|| {
                            unverified(format!("{v} owns a reference, but has no value"))
                        })?;
                        out.extend(std::iter::repeat_n(value, n as usize));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Fails if object `i`, which is being freed, holds an undefined value,
    /// which dropping what it holds would use. An array's elements that
    /// nothing set are fine: backends zero them.
    fn no_undefined(&self, i: usize) -> Step<()> {
        let object = &self.heap[i];
        if !matches!(object, Object::Array { .. }) && object.values().contains(&Value::Unset) {
            return Err(ErrorKind::Undefined(
                "drop of an object that holds an undefined value".into(),
            ));
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
                self.no_undefined(i)?;
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
    /// resumed can reach: they were lost with it, since it was never
    /// dropped either, which would have dropped what its frames own.
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
        let counted = |list: &[(ObjId, u32)], sign: char| -> Vec<String> {
            (list.iter())
                .map(|(o, n)| format!("#{} ({sign}{n})", o.0))
                .collect()
        };
        let plain =
            |list: &[ObjId]| -> Vec<String> { list.iter().map(|o| format!("#{}", o.0)).collect() };
        let sections = [
            ("undropped", counted(&self.undropped, '+')),
            ("dropped too often", counted(&self.overdropped, '-')),
            ("freed but referred to", plain(&self.dangling)),
            ("lost with continuations", counted(&self.lost, '+')),
            ("kept alive by cycles", plain(&self.cycles)),
        ];
        let parts: Vec<String> = (sections.iter())
            .filter(|(_, objs)| !objs.is_empty())
            .map(|(what, objs)| format!("{what}: {}", objs.join(", ")))
            .collect();
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
