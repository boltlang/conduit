use std::collections::HashMap;
use std::rc::Rc;

use conduit_ir::verify::{inherit, resolve};
use conduit_ir::{
    Atom, Block, CtorId, EffectId, Extern, ExternId, Func, FuncId, HandlerId, JoinDef, Label,
    Mutability, Op, Program, SourceId, Tag, Tail, Ty, Var,
};

use crate::error::{Error, ErrorKind, unverified};
use crate::prim;
use crate::value::{Continuation, ObjId, Object, Value};

/// Provides the functions of [`Op::CallExtern`].
pub trait Host {
    fn call(&mut self, id: ExternId, decl: &Extern, args: &[Value]) -> Result<Vec<Value>, String>;
}

/// A host without functions: every extern call fails.
pub struct NoHost;

impl Host for NoHost {
    fn call(&mut self, _: ExternId, decl: &Extern, _: &[Value]) -> Result<Vec<Value>, String> {
        Err(format!("no host function {}.{}", decl.module, decl.name))
    }
}

/// The join definitions of a function, by label.
type Labels<'p> = Rc<[Option<&'p JoinDef>]>;

#[derive(Debug)]
pub(crate) enum Frame<'p> {
    Code(Code<'p>),
    /// Pushed by `handle`, below the frames of its body.
    Handler {
        handler: HandlerId,
        env: Vec<Value>,
    },
}

/// A running function.
#[derive(Debug)]
pub(crate) struct Code<'p> {
    func: FuncId,
    labels: Labels<'p>,
    vars: Vec<Option<Value>>,
    /// What runs next.
    at: At<'p>,
    /// The variables of a `let` that waits for a call, `perform`, `resume`
    /// or `handle` to give its results, and where to go on after it.
    waiting: Option<(&'p [Var], At<'p>)>,
}

/// A place in a block: the `let` at `index`, or the tail once `index`
/// reaches the end of the `let`s.
#[derive(Debug, Clone, Copy)]
pub(crate) struct At<'p> {
    block: &'p Block,
    index: usize,
}

impl<'p> At<'p> {
    fn start(block: &'p Block) -> At<'p> {
        At { block, index: 0 }
    }

    fn next(self) -> At<'p> {
        At {
            index: self.index + 1,
            ..self
        }
    }
}

type Step<T> = Result<T, ErrorKind>;

/// Runs Conduit IR from a [`Program`].
pub struct Machine<'p, P: ?Sized, H> {
    p: &'p P,
    pub host: H,
    heap: Vec<Object<'p>>,
    stack: Vec<Frame<'p>>,
    labels: HashMap<FuncId, Labels<'p>>,
    /// The most frames the stack can have, handler frames included.
    pub max_frames: usize,
    /// How many more steps the program can run, or `None` for no limit.
    pub fuel: Option<u64>,
    source: Option<SourceId>,
}

impl<'p, P: Program + ?Sized, H: Host> Machine<'p, P, H> {
    pub fn new(p: &'p P, host: H) -> Self {
        Machine {
            p,
            host,
            heap: Vec::new(),
            stack: Vec::new(),
            labels: HashMap::new(),
            max_frames: 100_000,
            fuel: None,
            source: None,
        }
    }

    /// Calls a function and runs it to completion.
    pub fn call(&mut self, func: FuncId, args: Vec<Value>) -> Result<Vec<Value>, Error> {
        if !self.stack.is_empty() {
            return Err(self.error(unverified("the machine is already running")));
        }
        self.source = None;
        let result = self.enter(func, args).and_then(|()| self.run());
        result.map_err(|kind| {
            let error = self.error(kind);
            self.stack.clear();
            error
        })
    }

    pub fn object(&self, id: ObjId) -> Option<&Object<'p>> {
        self.heap.get(id.0 as usize)
    }

    /// Adds an object to the heap, for example to pass one to a function.
    pub fn alloc(&mut self, object: Object<'p>) -> Value {
        Value::Obj(self.alloc_id(object))
    }

    fn alloc_id(&mut self, object: Object<'p>) -> ObjId {
        self.heap.push(object);
        ObjId(self.heap.len() as u32 - 1)
    }

    fn error(&self, kind: ErrorKind) -> Error {
        let func = self.stack.iter().rev().find_map(|frame| match frame {
            Frame::Code(code) => Some(code.func),
            Frame::Handler { .. } => None,
        });
        Error {
            kind,
            func,
            source: self.source,
        }
    }

    fn run(&mut self) -> Step<Vec<Value>> {
        loop {
            if let Some(fuel) = &mut self.fuel {
                if *fuel == 0 {
                    return Err(ErrorKind::OutOfFuel);
                }
                *fuel -= 1;
            }
            if let Some(results) = self.step()? {
                return Ok(results);
            }
        }
    }

    /// Runs one `let` or tail. Gives the results once the first function
    /// returns.
    fn step(&mut self) -> Step<Option<Vec<Value>>> {
        let at = self.code()?.at;
        if let Some(l) = at.block.lets.get(at.index) {
            self.source = l.source;
            let args = self.atoms(&l.args)?;
            self.op(&l.op, &l.args, args, &l.binds, at.next())?;
            return Ok(None);
        }
        match &at.block.tail {
            // Join definitions are found through the function's labels.
            Tail::Join { body, .. } => self.code()?.at = At::start(body),
            Tail::Jump {
                target,
                args,
                source,
            } => {
                self.source = *source;
                let args = self.atoms(args)?;
                if *target == Label::RET {
                    return self.ret(args);
                }
                let code = self.code()?;
                let def = code.labels.get(target.0 as usize).copied().flatten();
                let def = def.ok_or_else(|| unverified(format!("{target} isn't defined")))?;
                bind(code, &def.params, args)?;
                code.at = At::start(&def.body);
            }
            Tail::Case {
                scrut,
                arms,
                default,
            } => {
                let tag = match self.atom(scrut)? {
                    Value::I32(n) => Tag::Int(n.into()),
                    Value::I64(n) => Tag::Int(n),
                    Value::Obj(id) => match self.object_ref(id)? {
                        Object::Data { ctor, .. } => Tag::Ctor(*ctor),
                        _ => return Err(unverified("case on an object that isn't data")),
                    },
                    _ => return Err(unverified("case on a float")),
                };
                let arm = arms.iter().find(|arm| arm.tag == tag).map(|arm| &arm.body);
                let next = arm.or(default.as_deref());
                let next = next.ok_or_else(|| unverified("no arm of a case matches"))?;
                self.code()?.at = At::start(next);
            }
            Tail::Unreachable => return Err(ErrorKind::Unreachable),
        }
        Ok(None)
    }

    fn code(&mut self) -> Step<&mut Code<'p>> {
        match self.stack.last_mut() {
            Some(Frame::Code(code)) => Ok(code),
            _ => Err(unverified("no code is running")),
        }
    }

    fn code_ref(&self) -> Step<&Code<'p>> {
        match self.stack.last() {
            Some(Frame::Code(code)) => Ok(code),
            _ => Err(unverified("no code is running")),
        }
    }

    fn atom(&self, a: &Atom) -> Step<Value> {
        match a {
            Atom::Lit(lit) => Ok((*lit).into()),
            Atom::Var(v) => {
                let value = self.code_ref()?.vars.get(v.0 as usize).copied().flatten();
                value.ok_or_else(|| unverified(format!("{v} has no value")))
            }
        }
    }

    fn atoms(&self, atoms: &[Atom]) -> Step<Vec<Value>> {
        atoms.iter().map(|a| self.atom(a)).collect()
    }

    fn object_ref(&self, id: ObjId) -> Step<&Object<'p>> {
        self.heap
            .get(id.0 as usize)
            .ok_or_else(|| unverified("dangling object"))
    }

    fn object_mut(&mut self, id: ObjId) -> Step<&mut Object<'p>> {
        self.heap
            .get_mut(id.0 as usize)
            .ok_or_else(|| unverified("dangling object"))
    }

    /// Pushes a frame for a call.
    fn enter(&mut self, func: FuncId, args: Vec<Value>) -> Step<()> {
        let f = self
            .p
            .func(func)
            .ok_or_else(|| unverified(format!("unknown function {func}")))?;
        if self.stack.len() >= self.max_frames {
            return Err(ErrorKind::StackOverflow);
        }
        let labels = self
            .labels
            .entry(func)
            .or_insert_with(|| join_defs(f))
            .clone();
        let mut code = Code {
            func,
            labels,
            vars: vec![None; f.vars.len()],
            at: At::start(&f.body),
            waiting: None,
        };
        bind(&mut code, &f.params, args)?;
        self.stack.push(Frame::Code(code));
        Ok(())
    }

    /// Returns from the running function.
    fn ret(&mut self, results: Vec<Value>) -> Step<Option<Vec<Value>>> {
        self.stack.pop();
        match self.stack.pop() {
            None => return Ok(Some(results)),
            // The body of a `handle` returned: run the return clause, which
            // returns to the `handle`.
            Some(Frame::Handler { handler, mut env }) => {
                let decl = self
                    .p
                    .handler(handler)
                    .ok_or_else(|| unverified(format!("unknown handler {handler}")))?;
                env.extend(results);
                self.enter(decl.ret, env)?;
            }
            Some(frame @ Frame::Code(_)) => {
                self.stack.push(frame);
                self.deliver(results)?;
            }
        }
        Ok(None)
    }

    /// Gives results to the `let` the running function waits on.
    fn deliver(&mut self, results: Vec<Value>) -> Step<()> {
        let code = self.code()?;
        let (binds, next) = code
            .waiting
            .take()
            .ok_or_else(|| unverified("results for code that doesn't wait"))?;
        bind(code, binds, results)?;
        code.at = next;
        Ok(())
    }

    /// Calls a function from a `let`, replacing the running function if
    /// the `let` only returns the results.
    fn call_from(
        &mut self,
        func: FuncId,
        args: Vec<Value>,
        binds: &'p [Var],
        next: At<'p>,
    ) -> Step<()> {
        self.wait_or_leave(binds, next)?;
        self.enter(func, args)
    }

    /// Before control leaves the running function from a `let`: makes it
    /// wait for the `let`'s results, or, if the `let` only returns them,
    /// removes the function so the results go straight to its caller.
    fn wait_or_leave(&mut self, binds: &'p [Var], next: At<'p>) -> Step<()> {
        if is_tail(binds, next) {
            self.stack.pop();
        } else {
            self.code()?.waiting = Some((binds, next));
        }
        Ok(())
    }

    /// Whether the variable an op binds is `Mut`, for ops that create an
    /// object of the variable's mutability.
    fn binds_mut(&self, binds: &[Var]) -> Step<bool> {
        Ok(self.bind_ty(binds)?.mutability() == Some(Mutability::Mut))
    }

    /// The type of the variable an op that gives one value binds.
    fn bind_ty(&self, binds: &[Var]) -> Step<Ty> {
        let code = self.code_ref()?;
        let f = self
            .p
            .func(code.func)
            .ok_or_else(|| unverified("unknown function"))?;
        let ty = binds.first().and_then(|b| f.vars.get(b.0 as usize));
        ty.cloned()
            .ok_or_else(|| unverified("an op that gives a value binds nothing"))
    }

    /// The type of an argument, as the function running sees it.
    fn atom_ty(&self, a: &Atom) -> Step<Ty> {
        match a {
            Atom::Lit(l) => Ok(l.ty()),
            Atom::Var(v) => {
                let code = self.code_ref()?;
                let f = self
                    .p
                    .func(code.func)
                    .ok_or_else(|| unverified("unknown function"))?;
                f.vars
                    .get(v.0 as usize)
                    .cloned()
                    .ok_or_else(|| unverified(format!("{v} has no type")))
            }
        }
    }

    fn op(
        &mut self,
        op: &'p Op,
        atoms: &[Atom],
        args: Vec<Value>,
        binds: &'p [Var],
        next: At<'p>,
    ) -> Step<()> {
        let results = match op {
            Op::Prim(prim) => vec![prim::eval(prim, &args)?],
            Op::Call(f) => return self.call_from(*f, args, binds, next),
            Op::CallExtern(e) => {
                let decl = self
                    .p
                    .extern_fn(*e)
                    .ok_or_else(|| unverified(format!("unknown extern {e}")))?;
                self.host.call(*e, decl, &args).map_err(ErrorKind::Host)?
            }
            Op::Closure(f) => {
                let is_mut = self.binds_mut(binds)?;
                vec![self.alloc(Object::Closure {
                    func: *f,
                    captures: args,
                    is_mut,
                })]
            }
            Op::Apply => {
                let (closure, rest) = args
                    .split_first()
                    .ok_or_else(|| unverified("apply without a closure"))?;
                let (func, mut all) = match self.object_ref(obj(*closure)?)? {
                    Object::Closure { func, captures, .. } => (*func, captures.clone()),
                    _ => return Err(unverified("apply of something that isn't a closure")),
                };
                all.extend_from_slice(rest);
                return self.call_from(func, all, binds, next);
            }
            Op::New(c) => {
                let is_mut = self.binds_mut(binds)?;
                vec![self.alloc(Object::Data {
                    ctor: *c,
                    fields: args,
                    is_mut,
                })]
            }
            Op::Get(c, i) => match self.object_ref(obj(arg(&args, 0)?)?)? {
                Object::Data { ctor, fields, .. } if ctor == c => {
                    vec![
                        *fields
                            .get(*i as usize)
                            .ok_or_else(|| unverified(format!("{c} has no field {i}")))?,
                    ]
                }
                _ => {
                    return Err(unverified(format!(
                        "get of {c} on an object it didn't build"
                    )));
                }
            },
            Op::Set(c, i) => {
                let value = arg(&args, 1)?;
                match self.object_mut(obj(arg(&args, 0)?)?)? {
                    Object::Data {
                        ctor,
                        fields,
                        is_mut,
                    } if ctor == c => {
                        if !*is_mut {
                            return Err(ErrorKind::WriteToImm);
                        }
                        let field = fields
                            .get_mut(*i as usize)
                            .ok_or_else(|| unverified(format!("{c} has no field {i}")))?;
                        *field = value;
                    }
                    _ => {
                        return Err(unverified(format!(
                            "set of {c} on an object it didn't build"
                        )));
                    }
                }
                vec![]
            }
            Op::ArrayNew => {
                let is_mut = self.binds_mut(binds)?;
                vec![self.alloc(Object::Array {
                    elems: args,
                    is_mut,
                })]
            }
            Op::ArrayFill => {
                let (Value::I32(len), value) = (arg(&args, 0)?, arg(&args, 1)?) else {
                    return Err(unverified("array fill needs a length"));
                };
                let is_mut = self.binds_mut(binds)?;
                // Lengths are unsigned, as in Wasm, but an array is at most
                // `i32::MAX` long, so that `ArrayLen` can give its length.
                if len < 0 {
                    return Err(out_of_bounds(len as u32 as usize, i32::MAX as usize));
                }
                let len = len as usize;
                vec![self.alloc(Object::Array {
                    elems: vec![value; len],
                    is_mut,
                })]
            }
            Op::ArrayGet => {
                let index = index(arg(&args, 1)?)?;
                match self.object_ref(obj(arg(&args, 0)?)?)? {
                    Object::Array { elems, .. } => vec![
                        *elems
                            .get(index)
                            .ok_or_else(|| out_of_bounds(index, elems.len()))?,
                    ],
                    _ => return Err(unverified("array get on something that isn't an array")),
                }
            }
            Op::ArraySet => {
                let (index, value) = (index(arg(&args, 1)?)?, arg(&args, 2)?);
                match self.object_mut(obj(arg(&args, 0)?)?)? {
                    Object::Array { elems, is_mut } => {
                        if !*is_mut {
                            return Err(ErrorKind::WriteToImm);
                        }
                        let len = elems.len();
                        *elems
                            .get_mut(index)
                            .ok_or_else(|| out_of_bounds(index, len))? = value;
                    }
                    _ => return Err(unverified("array set on something that isn't an array")),
                }
                vec![]
            }
            Op::ArrayLen => match self.object_ref(obj(arg(&args, 0)?)?)? {
                Object::Array { elems, .. } => vec![Value::I32(elems.len() as i32)],
                _ => return Err(unverified("array len on something that isn't an array")),
            },
            Op::RefNew => vec![self.alloc(Object::Ref(arg(&args, 0)?))],
            Op::RefGet => match self.object_ref(obj(arg(&args, 0)?)?)? {
                Object::Ref(value) => vec![*value],
                _ => return Err(unverified("ref get on something that isn't a ref")),
            },
            Op::RefSet => {
                let value = arg(&args, 1)?;
                match self.object_mut(obj(arg(&args, 0)?)?)? {
                    Object::Ref(slot) => *slot = value,
                    _ => return Err(unverified("ref set on something that isn't a ref")),
                }
                vec![]
            }
            Op::Box(ty) => vec![self.alloc(Object::Any {
                ty: ty.clone(),
                value: arg(&args, 0)?,
            })],
            Op::Unbox(ty) => match self.object_ref(obj(arg(&args, 0)?)?)? {
                Object::Any { ty: boxed, value } if boxed.is_subtype(ty) => vec![*value],
                Object::Any { ty: boxed, .. } => {
                    return Err(ErrorKind::Undefined(format!(
                        "unbox of a {boxed} as a {ty}"
                    )));
                }
                _ => return Err(unverified("unbox of something that isn't boxed")),
            },
            // The type of what it binds says what's copied.
            Op::Copy(_) => {
                let from =
                    self.atom_ty(atoms.first().ok_or_else(|| unverified("copy of nothing"))?)?;
                let to = self.bind_ty(binds)?;
                vec![self.copy(arg(&args, 0)?, &from, &to)?]
            }
            Op::Freeze => {
                let value = arg(&args, 0)?;
                self.freeze(value)?;
                vec![value]
            }
            Op::Handle(h) => return self.handle(*h, args, binds, next),
            Op::Perform(e, i) => return self.perform(*e, *i, args, binds, next),
            Op::Resume => return self.resume(args, binds, next),
        };
        let code = self.code()?;
        bind(code, binds, results)?;
        code.at = next;
        Ok(())
    }

    fn handle(
        &mut self,
        handler: HandlerId,
        mut args: Vec<Value>,
        binds: &'p [Var],
        next: At<'p>,
    ) -> Step<()> {
        let closure = args
            .pop()
            .ok_or_else(|| unverified("handle without a body"))?;
        let (func, captures) = match self.object_ref(obj(closure)?)? {
            Object::Closure { func, captures, .. } => (*func, captures.clone()),
            _ => return Err(unverified("the body of a handle isn't a closure")),
        };
        self.code()?.waiting = Some((binds, next));
        self.stack.push(Frame::Handler { handler, env: args });
        self.enter(func, captures)
    }

    fn perform(
        &mut self,
        effect: EffectId,
        op: u32,
        args: Vec<Value>,
        binds: &'p [Var],
        next: At<'p>,
    ) -> Step<()> {
        let p = self.p;
        let handles = |frame: &Frame| match frame {
            Frame::Handler { handler, .. } => {
                p.handler(*handler).is_some_and(|d| d.effect == effect)
            }
            Frame::Code(_) => false,
        };
        let at = self
            .stack
            .iter()
            .rposition(handles)
            .ok_or(ErrorKind::Unhandled(effect))?;
        self.code()?.waiting = Some((binds, next));
        // The continuation is the handler's frame and everything above it,
        // so that the resumed computation is handled again.
        let frames = self.stack.split_off(at);
        let Some(Frame::Handler { handler, env }) = frames.first() else {
            return Err(unverified("no handler frame"));
        };
        let decl = p
            .handler(*handler)
            .ok_or_else(|| unverified(format!("unknown handler {handler}")))?;
        let clause = *decl
            .ops
            .get(op as usize)
            .ok_or_else(|| unverified(format!("{handler} has no clause {op}")))?;
        let mut clause_args = env.clone();
        clause_args.extend(args);
        clause_args.push(self.alloc(Object::Cont(Continuation {
            frames: Some(frames),
        })));
        // The clause returns to the `handle`.
        self.enter(clause, clause_args)
    }

    fn resume(&mut self, args: Vec<Value>, binds: &'p [Var], next: At<'p>) -> Step<()> {
        let (k, values) = args
            .split_first()
            .ok_or_else(|| unverified("resume without a continuation"))?;
        let frames = match self.object_mut(obj(*k)?)? {
            Object::Cont(cont) => cont
                .frames
                .take()
                .ok_or(ErrorKind::ContinuationResumedTwice)?,
            _ => return Err(unverified("resume of something that isn't a continuation")),
        };
        // The resumed computation returns, through the handler's return
        // clause, to this `let`.
        self.wait_or_leave(binds, next)?;
        if self.stack.len() + frames.len() > self.max_frames {
            return Err(ErrorKind::StackOverflow);
        }
        self.stack.extend(frames);
        self.deliver(values.to_vec())
    }

    /// `Copy`: turns a value of type `from` into one of type `to`, which
    /// differ only in mutabilities. Where the two types are equal, the value
    /// is kept with what it holds; where they differ, a data object or
    /// array is copied to the mutability `to` gives, and its fields and
    /// elements are walked the same way. The types decide, not the objects,
    /// as in compiled code, which doesn't know an object's mutability. Each
    /// object is copied once, so sharing and cycles stay the same.
    fn copy(&mut self, value: Value, from: &Ty, to: &Ty) -> Step<Value> {
        let mut copies = HashMap::new();
        // Copies whose children are still the originals', with the types of
        // the original and of the copy.
        let mut todo = Vec::new();
        let root = self.copy_one(value, from, to, &mut copies, &mut todo)?;
        while let Some((id, from, to)) = todo.pop() {
            let children: Vec<(Value, Ty, Ty)> = match (self.object_ref(id)?, &from, &to) {
                (Object::Data { ctor, fields, .. }, Ty::Data(_, m1, a1), Ty::Data(_, m2, a2)) => {
                    let tys = self.field_tys(*ctor, fields.len())?;
                    let pairs = tys
                        .iter()
                        .map(|field| (resolve(field, *m1, a1), resolve(field, *m2, a2)));
                    fields
                        .iter()
                        .zip(pairs)
                        .map(|(&v, (from, to))| (v, from, to))
                        .collect()
                }
                (Object::Array { elems, .. }, Ty::Array(e1, m1), Ty::Array(e2, m2)) => {
                    let (from, to) = (inherit(e1, *m1), inherit(e2, *m2));
                    elems
                        .iter()
                        .map(|&v| (v, from.clone(), to.clone()))
                        .collect()
                }
                _ => return Err(unverified("a copy of an object of another type")),
            };
            let mut copied = Vec::with_capacity(children.len());
            for (value, from, to) in children {
                copied.push(self.copy_one(value, &from, &to, &mut copies, &mut todo)?);
            }
            match self.object_mut(id)? {
                Object::Data { fields, .. } => *fields = copied,
                Object::Array { elems, .. } => *elems = copied,
                _ => {}
            }
        }
        Ok(root)
    }

    /// The copy of one value of type `from` as a `to`: the value itself if
    /// the types are equal, otherwise a new object.
    fn copy_one(
        &mut self,
        value: Value,
        from: &Ty,
        to: &Ty,
        copies: &mut HashMap<ObjId, ObjId>,
        todo: &mut Vec<(ObjId, Ty, Ty)>,
    ) -> Step<Value> {
        let Value::Obj(id) = value else {
            return Ok(value);
        };
        if from == to || !matches!(to, Ty::Data(..) | Ty::Array(..)) {
            // Kept, or a closure, ref, `any` or continuation, which `Copy`
            // doesn't look into.
            return Ok(value);
        }
        if let Some(&copy) = copies.get(&id) {
            return Ok(Value::Obj(copy));
        }
        let is_mut = to.mutability() == Some(Mutability::Mut);
        let copy = match self.object_ref(id)? {
            Object::Data { ctor, fields, .. } => Object::Data {
                ctor: *ctor,
                fields: fields.clone(),
                is_mut,
            },
            Object::Array { elems, .. } => Object::Array {
                elems: elems.clone(),
                is_mut,
            },
            _ => return Err(unverified("a copy of an object of another type")),
        };
        let copy = self.alloc_id(copy);
        copies.insert(id, copy);
        todo.push((copy, from.clone(), to.clone()));
        Ok(Value::Obj(copy))
    }

    /// `Freeze`: makes `Imm` every object `Copy(Imm)` would copy, instead
    /// of copying it.
    fn freeze(&mut self, value: Value) -> Step<()> {
        let mut todo = vec![value];
        while let Some(value) = todo.pop() {
            let Value::Obj(id) = value else { continue };
            match self.object_mut(id)? {
                Object::Data { is_mut, .. } | Object::Array { is_mut, .. } if *is_mut => {
                    *is_mut = false;
                }
                // Already `Imm`, so it holds nothing `Mut`.
                _ => continue,
            }
            // A valid `Freeze` leaves nothing `Mut` that its result reaches,
            // so everything it holds can be followed.
            if let Some(values) = self.children(id)? {
                todo.extend(values);
            }
        }
        Ok(())
    }

    /// The fields or elements of a data object or array.
    fn children(&self, id: ObjId) -> Step<Option<Vec<Value>>> {
        Ok(match self.object_ref(id)? {
            Object::Data { fields, .. } => Some(fields.clone()),
            Object::Array { elems, .. } => Some(elems.clone()),
            _ => None,
        })
    }

    /// The declared field types of constructor `c`, for an object with `n`
    /// fields.
    fn field_tys(&self, c: CtorId, n: usize) -> Step<&'p [Ty]> {
        let p = self.p;
        let decl = p
            .data(c.data)
            .ok_or_else(|| unverified(format!("unknown data type {}", c.data)))?;
        let tys = &decl
            .ctors
            .get(c.index as usize)
            .ok_or_else(|| unverified("unknown constructor"))?
            .fields;
        if tys.len() != n {
            return Err(unverified(format!("{c} has the wrong number of fields")));
        }
        Ok(tys)
    }
}

fn bind(code: &mut Code, vars: &[Var], values: Vec<Value>) -> Step<()> {
    if vars.len() != values.len() {
        return Err(unverified(format!(
            "{} values for {} variables",
            values.len(),
            vars.len()
        )));
    }
    for (var, value) in vars.iter().zip(values) {
        let slot = code
            .vars
            .get_mut(var.0 as usize)
            .ok_or_else(|| unverified(format!("{var} has no type")))?;
        *slot = Some(value);
    }
    Ok(())
}

/// Whether what follows a `let`, at `next`, only returns what the `let`
/// binds.
fn is_tail(binds: &[Var], next: At) -> bool {
    match &next.block.tail {
        Tail::Jump {
            target: Label::RET,
            args,
            ..
        } if next.index == next.block.lets.len() => {
            args.len() == binds.len() && args.iter().zip(binds).all(|(a, b)| *a == Atom::Var(*b))
        }
        _ => false,
    }
}

fn arg(args: &[Value], i: usize) -> Step<Value> {
    args.get(i)
        .copied()
        .ok_or_else(|| unverified(format!("missing argument {i}")))
}

fn obj(value: Value) -> Step<ObjId> {
    match value {
        Value::Obj(id) => Ok(id),
        _ => Err(unverified("a scalar where an object is expected")),
    }
}

fn index(value: Value) -> Step<usize> {
    match value {
        // Indices are unsigned, as in Wasm.
        Value::I32(i) => Ok(i as u32 as usize),
        _ => Err(unverified("an index that isn't an i32")),
    }
}

fn out_of_bounds(index: usize, len: usize) -> ErrorKind {
    ErrorKind::OutOfBounds {
        index: index as u64,
        len: len as u64,
    }
}

/// Finds every join definition of a function, without recursion.
fn join_defs(f: &Func) -> Labels<'_> {
    let mut table = vec![None; f.labels as usize];
    let mut todo = vec![&f.body];
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
            Tail::Jump { .. } | Tail::Unreachable => {}
        }
    }
    table.into()
}
