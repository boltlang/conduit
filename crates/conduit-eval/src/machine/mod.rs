use std::collections::HashMap;
use std::rc::Rc;

use conduit_ir::Link;
use conduit_ir::types::subst_params;
use conduit_ir::{
    Atom, Block, ConstId, ConstValue, EffectId, Extern, Func, FuncId, HandlerId, JoinDef, Label,
    Mutability, Op, Program, Row, Sig, SourceId, Tag, Tail, Ty, Var,
};

use crate::error::{Error, ErrorKind, HostError, unverified};
use crate::prim;
use crate::value::{Continuation, ObjId, Object, Value};

mod counts;
mod cycles;

pub use counts::{Count, Leaks, Stats};
use cycles::Color;

/// Provides the functions of [`Op::CallExtern`]: those of the externs
/// with a `#[link(emu, ..)]`, by its name. An extern with another kind of
/// link is for another target, and calling it is an error.
pub trait Host {
    /// Calls the function named `name`, extern `decl`, with `args`.
    /// Through `arrays` it can read the arrays it's given, and write to the
    /// `mut` ones. A message converts to [`HostError::Failed`], so `?` on
    /// one works.
    fn call(
        &mut self,
        name: &str,
        decl: &Extern,
        args: &[Value],
        arrays: &mut Arrays<'_, '_>,
    ) -> Result<Vec<Value>, HostError>;
}

/// The arrays of the heap, as a host function sees them.
pub struct Arrays<'a, 'p> {
    heap: &'a mut [Object<'p>],
}

impl Arrays<'_, '_> {
    /// The elements of array `v`, or `None` if it isn't an array.
    pub fn get(&self, v: &Value) -> Option<&[Value]> {
        match v {
            Value::Obj(id) => match self.heap.get(id.0 as usize)? {
                Object::Array { elems, .. } => Some(elems),
                _ => None,
            },
            _ => None,
        }
    }

    /// The elements of array `v` to write to, or `None` if it isn't a
    /// `mut` array. The host must write values of the element type.
    pub fn get_mut(&mut self, v: &Value) -> Option<&mut [Value]> {
        match v {
            Value::Obj(id) => match self.heap.get_mut(id.0 as usize)? {
                Object::Array {
                    elems,
                    is_mut: true,
                } => Some(elems),
                _ => None,
            },
            _ => None,
        }
    }
}

/// A host without functions: every extern call fails.
pub struct NoHost;

impl Host for NoHost {
    fn call(
        &mut self,
        name: &str,
        _: &Extern,
        _: &[Value],
        _: &mut Arrays<'_, '_>,
    ) -> Result<Vec<Value>, HostError> {
        Err(format!("no host function {name}").into())
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
    /// The function's mutability arguments, each `Imm` or `Mut`.
    margs: Vec<Mutability>,
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
    /// The count of each object in `heap`, kept even when the machine
    /// doesn't count, so that nothing is ever freed then.
    counts: Vec<Count>,
    stack: Vec<Frame<'p>>,
    labels: HashMap<FuncId, Labels<'p>>,
    /// The constants made so far, made the first time they're used, or
    /// `None` while one is being made.
    consts: HashMap<ConstId, Option<Value>>,
    /// The most frames the stack can have, handler frames included.
    pub max_frames: usize,
    /// How many more steps the program can run, or `None` for no limit.
    pub fuel: Option<u64>,
    /// Whether to count references: `dup` and `drop` change counts and
    /// free, and the ops that make or remove references inside objects
    /// count them (see "Reference counts" in `docs/ir.md`). Off, `dup` and
    /// `drop` do nothing, and nothing is freed.
    pub counting: bool,
    /// Whether to collect cycles too, with counting, by trial deletion
    /// (Bacon and Rajan, "Concurrent Cycle Collection in Reference Counted
    /// Systems", the synchronous version): an object that may be in a
    /// cycle and that a `drop` leaves above 0 is buffered, and the buffer
    /// is collected once it holds [`cycle_buffer`](Self::cycle_buffer)
    /// objects, or by [`collect_cycles`](Self::collect_cycles). `Imm`
    /// objects are never buffered: they can't be in a cycle.
    pub cycles: bool,
    /// How many objects the buffer of possible cycles holds before it's
    /// collected.
    pub cycle_buffer: usize,
    /// The color of each object in trial deletion, and whether it's
    /// buffered.
    colors: Vec<Color>,
    buffered: Vec<bool>,
    /// The objects that may be the roots of garbage cycles.
    roots: Vec<ObjId>,
    /// What the machine has done so far, for benchmarks. Every number is
    /// the same each time a program runs.
    pub stats: Stats,
    /// How many objects are allocated and not freed.
    live: u64,
    source: Option<SourceId>,
}

impl<'p, P: Program + ?Sized, H: Host> Machine<'p, P, H> {
    pub fn new(p: &'p P, host: H) -> Self {
        Machine {
            p,
            host,
            heap: Vec::new(),
            counts: Vec::new(),
            stack: Vec::new(),
            labels: HashMap::new(),
            consts: HashMap::new(),
            max_frames: 100_000,
            fuel: None,
            counting: false,
            cycles: false,
            cycle_buffer: 10_000,
            colors: Vec::new(),
            buffered: Vec::new(),
            roots: Vec::new(),
            stats: Stats::default(),
            live: 0,
            source: None,
        }
    }

    /// Calls a function and runs it to completion. When counting, it's
    /// called as from outside the module: it owns `args`, as an exported
    /// function does, and the caller owns what it gives.
    pub fn call(&mut self, func: FuncId, args: Vec<Value>) -> Result<Vec<Value>, Error> {
        self.call_at(func, Vec::new(), args)
    }

    /// Calls a function with mutability parameters, each argument `Imm` or
    /// `Mut`, and runs it to completion.
    pub fn call_at(
        &mut self,
        func: FuncId,
        margs: Vec<Mutability>,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, Error> {
        if !self.stack.is_empty() {
            return Err(self.error(unverified("the machine is already running")));
        }
        self.source = None;
        if let Some(m) = margs
            .iter()
            .find(|m| !matches!(m, Mutability::Imm | Mutability::Mut))
        {
            return Err(self.error(unverified(format!(
                "a mutability argument is imm or mut, not {m}"
            ))));
        }
        let result = self.enter(func, args, margs).and_then(|()| self.run());
        result.map_err(|kind| {
            let error = self.error(kind);
            self.stack.clear();
            error
        })
    }

    /// An object, unless it has been freed.
    pub fn object(&self, id: ObjId) -> Option<&Object<'p>> {
        match self.counts.get(id.0 as usize)? {
            Count::Freed | Count::Reserved => None,
            _ => self.heap.get(id.0 as usize),
        }
    }

    /// Every object made so far that hasn't been freed. Without
    /// [`counting`](Self::counting), nothing is freed, so after a run this
    /// is everything the run made, which tests can inspect.
    pub fn objects(&self) -> impl Iterator<Item = (ObjId, &Object<'p>)> {
        self.heap
            .iter()
            .zip(&self.counts)
            .enumerate()
            .filter(|(_, (_, c))| !matches!(c, Count::Freed | Count::Reserved))
            .map(|(i, (o, _))| (ObjId(i as u32), o))
    }

    /// Adds an object to the heap, for example to pass one to a function.
    pub fn alloc(&mut self, object: Object<'p>) -> Value {
        Value::Obj(self.alloc_id(object))
    }

    fn alloc_id(&mut self, object: Object<'p>) -> ObjId {
        self.stats.allocs += 1;
        self.live += 1;
        self.stats.peak_live = self.stats.peak_live.max(self.live);
        self.heap.push(object);
        self.counts.push(Count::Live(1));
        self.colors.push(Color::Black);
        self.buffered.push(false);
        ObjId(self.heap.len() as u32 - 1)
    }

    /// The object of a constant, made the first time it's asked for, so
    /// that every use gets the same one.
    fn constant(&mut self, id: ConstId) -> Step<Value> {
        match self.consts.get(&id) {
            Some(Some(value)) => return Ok(*value),
            Some(None) => return Err(unverified(format!("{id} refers to itself"))),
            None => {}
        }
        let decl = self
            .p
            .constant(id)
            .ok_or_else(|| unverified(format!("unknown constant {id}")))?;
        self.consts.insert(id, None);
        let value = self.const_value(&decl.value)?;
        self.consts.insert(id, Some(value));
        Ok(value)
    }

    fn const_value(&mut self, value: &'p ConstValue) -> Step<Value> {
        let all = |m: &mut Self, parts: &'p [ConstValue]| -> Step<Vec<Value>> {
            parts.iter().map(|v| m.const_value(v)).collect()
        };
        Ok(match value {
            ConstValue::Lit(lit) => (*lit).into(),
            ConstValue::Array(elems) => {
                let elems = all(self, elems)?;
                self.alloc_immortal(Object::Array {
                    elems,
                    is_mut: false,
                })
            }
            ConstValue::New(c, fields) => {
                let fields = all(self, fields)?;
                self.alloc_immortal(Object::Data {
                    ctor: *c,
                    fields,
                    is_mut: false,
                })
            }
            ConstValue::Const(id) => self.constant(*id)?,
        })
    }

    /// Allocates an object that's never counted, for a constant.
    fn alloc_immortal(&mut self, object: Object<'p>) -> Value {
        let id = self.alloc_id(object);
        // A constant isn't allocated by the program.
        self.stats.allocs -= 1;
        self.live -= 1;
        self.counts[id.0 as usize] = Count::Immortal;
        Value::Obj(id)
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
            self.stats.steps += 1;
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
            self.op(&l.op, args, &l.binds, at.next())?;
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
                    Value::I8(n) => Tag::Int(n.into()),
                    Value::I16(n) => Tag::Int(n.into()),
                    Value::I32(n) => Tag::Int(n.into()),
                    Value::I64(n) => Tag::Int(n),
                    Value::Obj(id) => match self.object_ref(id)? {
                        Object::Data { ctor, .. } => Tag::Ctor(*ctor),
                        _ => return Err(unverified("case on an object that isn't data")),
                    },
                    Value::Unset => return Err(undefined_use()),
                    _ => return Err(unverified("case on a float")),
                };
                let arm = arms.iter().find(|arm| arm.tag == tag).map(|arm| &arm.body);
                let next = arm.or(default.as_deref());
                let next = next.ok_or_else(|| unverified("no arm of a case matches"))?;
                self.code()?.at = At::start(next);
            }
            Tail::Unreachable => {
                return Err(ErrorKind::Undefined("reached unreachable code".into()));
            }
            Tail::Trap => return Err(ErrorKind::Trap),
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
        self.live(id)?;
        self.heap
            .get(id.0 as usize)
            .ok_or_else(|| unverified("dangling object"))
    }

    fn object_mut(&mut self, id: ObjId) -> Step<&mut Object<'p>> {
        self.live(id)?;
        self.heap
            .get_mut(id.0 as usize)
            .ok_or_else(|| unverified("dangling object"))
    }

    /// Fails if the object has been freed.
    fn live(&self, id: ObjId) -> Step<()> {
        match self.counts.get(id.0 as usize) {
            Some(Count::Freed | Count::Reserved) => Err(freed("use")),
            _ => Ok(()),
        }
    }

    /// Pushes a frame for a call.
    fn enter(&mut self, func: FuncId, args: Vec<Value>, margs: Vec<Mutability>) -> Step<()> {
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
        if margs.len() != f.mut_params.count as usize {
            return Err(unverified(format!(
                "{func} takes {} mutability arguments, but gets {}",
                f.mut_params.count,
                margs.len()
            )));
        }
        let mut code = Code {
            func,
            margs,
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
                self.enter(decl.ret, env, Vec::new())?;
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
        margs: Vec<Mutability>,
        args: Vec<Value>,
        binds: &'p [Var],
        next: At<'p>,
    ) -> Step<()> {
        self.wait_or_leave(binds, next)?;
        self.enter(func, args, margs)
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
        match self.bind_ty(binds)?.mutability() {
            Some(m) => Ok(self.concrete(m)? == Mutability::Mut),
            None => Ok(false),
        }
    }

    /// What a mutability is in the running function: a parameter is its
    /// argument.
    fn concrete(&self, m: Mutability) -> Step<Mutability> {
        match m {
            Mutability::Param(i) => {
                let code = self.code_ref()?;
                let arg = (i as usize).checked_sub(1).and_then(|j| code.margs.get(j));
                arg.copied()
                    .ok_or_else(|| unverified(format!("{m} isn't a parameter here")))
            }
            m => Ok(m),
        }
    }

    /// Mutability arguments of a call or closure, made concrete.
    fn concrete_all(&self, margs: &[Mutability]) -> Step<Vec<Mutability>> {
        margs.iter().map(|&m| self.concrete(m)).collect()
    }

    /// A type in the running function, with its parameters' arguments in
    /// place of them.
    fn concrete_ty(&self, ty: &Ty) -> Step<Ty> {
        Ok(erase_rows(&subst_params(ty, &self.code_ref()?.margs)))
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

    fn op(&mut self, op: &'p Op, args: Vec<Value>, binds: &'p [Var], next: At<'p>) -> Step<()> {
        if op.is_count() {
            self.stats.count_ops += 1;
        }
        let results = match op {
            Op::Prim(prim) => vec![prim::eval(prim, &args)?],
            Op::Call(f, margs, _) => {
                let margs = self.concrete_all(margs)?;
                return self.call_from(*f, margs, args, binds, next);
            }
            Op::CallExtern(e) => {
                let decl = self
                    .p
                    .extern_fn(*e)
                    .ok_or_else(|| unverified(format!("unknown extern {e}")))?;
                let Link::Emu { name } = &decl.link else {
                    let shown = decl
                        .debug_name
                        .as_deref()
                        .map_or(e.to_string(), str::to_string);
                    return Err(ErrorKind::Host(format!(
                        "{shown} has a {} link, for another target: the evaluator calls emu ones",
                        decl.link.kind()
                    )));
                };
                let mut arrays = Arrays {
                    heap: &mut self.heap,
                };
                (self.host)
                    .call(name, decl, &args, &mut arrays)
                    .map_err(ErrorKind::from)?
            }
            Op::Closure(f, margs, _) => {
                let is_mut = self.binds_mut(binds)?;
                let margs = self.concrete_all(margs)?;
                vec![self.alloc(Object::Closure {
                    func: *f,
                    margs,
                    captures: args,
                    is_mut,
                })]
            }
            Op::Apply => {
                let (closure, rest) = args
                    .split_first()
                    .ok_or_else(|| unverified("apply without a closure"))?;
                let (func, margs, mut all) = match self.object_ref(obj(*closure)?)? {
                    Object::Closure {
                        func,
                        margs,
                        captures,
                        ..
                    } => (*func, margs.clone(), captures.clone()),
                    _ => return Err(unverified("apply of something that isn't a closure")),
                };
                // It takes the closure, and the function owns its
                // parameters, the captures among them: as if the closure
                // gave up its captures, which it keeps if it's shared.
                self.dup_all(&all)?;
                self.drop_value(*closure)?;
                all.extend_from_slice(rest);
                return self.call_from(func, margs, all, binds, next);
            }
            Op::New(c) => {
                let is_mut = self.binds_mut(binds)?;
                vec![self.alloc(Object::Data {
                    ctor: *c,
                    fields: args,
                    is_mut,
                })]
            }
            Op::Const(id) => vec![self.constant(*id)?],
            Op::Undefined => vec![Value::Unset],
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
                        let old = std::mem::replace(field, value);
                        self.drop_value(old)?;
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
                // It takes one reference to the value and holds `len`.
                match len {
                    0 => self.drop_value(value)?,
                    n => self.dup_n(value, n as u32 - 1)?,
                }
                vec![self.alloc(Object::Array {
                    elems: vec![value; len],
                    is_mut,
                })]
            }
            Op::ArrayAlloc => {
                let Value::I32(len) = arg(&args, 0)? else {
                    return Err(unverified("array alloc needs a length"));
                };
                if len < 0 {
                    return Err(out_of_bounds(len as u32 as usize, i32::MAX as usize));
                }
                vec![self.alloc(Object::Array {
                    elems: vec![Value::Unset; len as usize],
                    is_mut: true,
                })]
            }
            Op::ArrayGet => {
                let index = index(arg(&args, 1)?)?;
                match self.object_ref(obj(arg(&args, 0)?)?)? {
                    Object::Array { elems, .. } => {
                        match *elems
                            .get(index)
                            .ok_or_else(|| out_of_bounds(index, elems.len()))?
                        {
                            Value::Unset => {
                                return Err(ErrorKind::Undefined(format!(
                                    "array get of element {index}, which isn't set"
                                )));
                            }
                            value => vec![value],
                        }
                    }
                    _ => return Err(unverified("array get on something that isn't an array")),
                }
            }
            Op::ArrayCopy => {
                let (dst, src) = (obj(arg(&args, 0)?)?, obj(arg(&args, 2)?)?);
                let (dst_start, src_start, count) = (
                    index(arg(&args, 1)?)?,
                    index(arg(&args, 3)?)?,
                    index(arg(&args, 4)?)?,
                );
                // The source's elements first, as through a temporary, so
                // that the two can be the same array.
                let elems = match self.object_ref(src)? {
                    Object::Array { elems, .. } => {
                        let end = src_start + count;
                        elems
                            .get(src_start..end)
                            .ok_or_else(|| out_of_bounds(end, elems.len()))?
                            .to_vec()
                    }
                    _ => return Err(unverified("array copy from something that isn't an array")),
                };
                match self.object_mut(dst)? {
                    Object::Array { elems: to, is_mut } => {
                        let end = dst_start + count;
                        let len = to.len();
                        let range = to
                            .get_mut(dst_start..end)
                            .ok_or_else(|| out_of_bounds(end, len))?;
                        if !*is_mut {
                            return Err(ErrorKind::WriteToImm);
                        }
                        let old = range.to_vec();
                        range.copy_from_slice(&elems);
                        // What it copies first, so that copying an
                        // element over itself doesn't free it.
                        self.dup_all(&elems)?;
                        old.into_iter().try_for_each(|v| self.drop_value(v))?;
                    }
                    _ => return Err(unverified("array copy into something that isn't an array")),
                }
                vec![]
            }
            Op::ArraySet => {
                let (index, value) = (index(arg(&args, 1)?)?, arg(&args, 2)?);
                match self.object_mut(obj(arg(&args, 0)?)?)? {
                    Object::Array { elems, is_mut } => {
                        if !*is_mut {
                            return Err(ErrorKind::WriteToImm);
                        }
                        let len = elems.len();
                        let slot = elems
                            .get_mut(index)
                            .ok_or_else(|| out_of_bounds(index, len))?;
                        let old = std::mem::replace(slot, value);
                        self.drop_value(old)?;
                    }
                    _ => return Err(unverified("array set on something that isn't an array")),
                }
                vec![]
            }
            Op::ArrayLen => match self.object_ref(obj(arg(&args, 0)?)?)? {
                Object::Array { elems, .. } => vec![Value::I32(elems.len() as i32)],
                _ => return Err(unverified("array len on something that isn't an array")),
            },
            Op::ArrayClone => {
                let elems = match self.object_ref(obj(arg(&args, 0)?)?)? {
                    Object::Array { elems, .. } => elems.clone(),
                    _ => return Err(unverified("array clone of something that isn't an array")),
                };
                let is_mut = self.binds_mut(binds)?;
                // The array is borrowed, and the new one shares its elements.
                self.dup_all(&elems)?;
                vec![self.alloc(Object::Array { elems, is_mut })]
            }
            Op::ArrayFreeze => {
                let array = arg(&args, 0)?;
                match self.object_mut(obj(array)?)? {
                    Object::Array { is_mut, .. } => *is_mut = false,
                    _ => return Err(unverified("array freeze of something that isn't an array")),
                }
                vec![array]
            }
            Op::RefNew => vec![self.alloc(Object::Ref(arg(&args, 0)?))],
            Op::RefGet => match self.object_ref(obj(arg(&args, 0)?)?)? {
                Object::Ref(value) => vec![*value],
                _ => return Err(unverified("ref get on something that isn't a ref")),
            },
            Op::RefSet => {
                let value = arg(&args, 1)?;
                match self.object_mut(obj(arg(&args, 0)?)?)? {
                    Object::Ref(slot) => {
                        let old = std::mem::replace(slot, value);
                        self.drop_value(old)?;
                    }
                    _ => return Err(unverified("ref set on something that isn't a ref")),
                }
                vec![]
            }
            Op::Box(ty) => {
                let ty = self.concrete_ty(ty)?;
                vec![self.alloc(Object::Any {
                    ty,
                    value: arg(&args, 0)?,
                })]
            }
            Op::Unbox(ty) => match self.object_ref(obj(arg(&args, 0)?)?)? {
                Object::Any { ty: boxed, value } if boxed.is_subtype(&self.concrete_ty(ty)?) => {
                    vec![*value]
                }
                Object::Any { ty: boxed, .. } => {
                    return Err(ErrorKind::Undefined(format!(
                        "unbox of a {boxed} as a {ty}"
                    )));
                }
                _ => return Err(unverified("unbox of something that isn't boxed")),
            },
            Op::Handle(h, _) => return self.handle(*h, args, binds, next),
            Op::Perform(e, i) => return self.perform(*e, *i, args, binds, next),
            Op::Resume => return self.resume(args, binds, next),
            Op::Discontinue(e, i) => return self.discontinue(*e, *i, args, binds, next),
            Op::Dup => {
                self.dup(arg(&args, 0)?)?;
                vec![]
            }
            Op::Drop => {
                self.drop_value(arg(&args, 0)?)?;
                vec![]
            }
            Op::IsUnique => vec![Value::I32(self.is_unique(arg(&args, 0)?)? as i32)],
            Op::Free => {
                self.free(arg(&args, 0)?)?;
                vec![]
            }
            Op::DecRef => {
                self.decref(arg(&args, 0)?)?;
                vec![]
            }
            Op::DropReuse => vec![self.drop_reuse(arg(&args, 0)?)?],
            Op::ReuseToken => {
                let id = obj(arg(&args, 0)?)?;
                if !self.counting {
                    vec![Value::Unset]
                } else {
                    match self.counts.get(id.0 as usize) {
                        Some(Count::Live(1)) => {
                            self.counts[id.0 as usize] = Count::Reserved;
                            vec![Value::Obj(id)]
                        }
                        Some(Count::Freed | Count::Reserved) => return Err(freed("reuse_token")),
                        _ => {
                            return Err(ErrorKind::Undefined(
                                "reuse_token of an object that isn't unique".into(),
                            ));
                        }
                    }
                }
            }
            Op::Reuse(c) => {
                let (token, fields) = args
                    .split_first()
                    .ok_or_else(|| unverified("reuse without a token"))?;
                let is_mut = self.binds_mut(binds)?;
                let object = Object::Data {
                    ctor: *c,
                    fields: fields.to_vec(),
                    is_mut,
                };
                match *token {
                    Value::Unset => {
                        self.stats.fresh += 1;
                        vec![self.alloc(object)]
                    }
                    Value::Obj(id) if self.counts.get(id.0 as usize) == Some(&Count::Reserved) => {
                        self.stats.reused += 1;
                        self.heap[id.0 as usize] = object;
                        self.counts[id.0 as usize] = Count::Live(1);
                        self.colors[id.0 as usize] = Color::Black;
                        vec![Value::Obj(id)]
                    }
                    _ => {
                        return Err(ErrorKind::Undefined(
                            "reuse of a token that holds no memory to reuse".into(),
                        ));
                    }
                }
            }
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
        let (func, margs, captures) = match self.object_ref(obj(closure)?)? {
            Object::Closure {
                func,
                margs,
                captures,
                ..
            } => (*func, margs.clone(), captures.clone()),
            _ => return Err(unverified("the body of a handle isn't a closure")),
        };
        // As for `apply`.
        self.dup_all(&captures)?;
        self.drop_value(closure)?;
        self.code()?.waiting = Some((binds, next));
        self.stack.push(Frame::Handler { handler, env: args });
        self.enter(func, captures, margs)
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
                p.handler(*handler).is_some_and(|d| d.handles(effect))
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
        let clause = decl
            .clause(effect, op)
            .ok_or_else(|| unverified(format!("{handler} has no clause {effect}.{op}")))?;
        let mut clause_args = env.clone();
        // The handler keeps its environment, and the clause gets its own.
        self.dup_all(&clause_args)?;
        clause_args.extend(args);
        clause_args.push(self.alloc(Object::Cont(Continuation {
            frames: Some(frames),
        })));
        // The clause returns to the `handle`.
        self.enter(clause, clause_args, Vec::new())
    }

    fn resume(&mut self, args: Vec<Value>, binds: &'p [Var], next: At<'p>) -> Step<()> {
        let values = self.reinstate(args, binds, next)?;
        self.deliver(values)
    }

    /// Puts the frames of the continuation back, like `resume`, and then
    /// performs `effect.op(args)` in the code that was waiting in them.
    fn discontinue(
        &mut self,
        effect: EffectId,
        op: u32,
        args: Vec<Value>,
        binds: &'p [Var],
        next: At<'p>,
    ) -> Step<()> {
        let args = self.reinstate(args, binds, next)?;
        let (binds, next) = self
            .code()?
            .waiting
            .take()
            .ok_or_else(|| unverified("a continuation that doesn't wait"))?;
        self.perform(effect, op, args, binds, next)
    }

    /// Takes the frames out of the continuation that `args` starts with and
    /// puts them back on the stack, so that what they give comes back to
    /// the `let` at `binds` and `next`. Gives the rest of `args`.
    fn reinstate(&mut self, args: Vec<Value>, binds: &'p [Var], next: At<'p>) -> Step<Vec<Value>> {
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
        // `resume` and `discontinue` take the continuation.
        self.drop_value(*k)?;
        // The resumed computation returns, through the handler's return
        // clause, to this `let`.
        self.wait_or_leave(binds, next)?;
        if self.stack.len() + frames.len() > self.max_frames {
            return Err(ErrorKind::StackOverflow);
        }
        self.stack.extend(frames);
        Ok(values.to_vec())
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
        Value::Unset => Err(undefined_use()),
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
            Tail::Jump { .. } | Tail::Unreachable | Tail::Trap => {}
        }
    }
    table.into()
}

/// `ty` with every row `Any`. Rows don't exist at run time, and a frame
/// doesn't know its function's row arguments, so `Box` and `Unbox` compare
/// types without them.
fn erase_rows(ty: &Ty) -> Ty {
    let sig = |sig: &Sig| Sig {
        row: Row::ANY,
        params: sig.params.iter().map(erase_rows).collect(),
        results: sig.results.iter().map(erase_rows).collect(),
    };
    match ty {
        Ty::Data(d, m, args, rows) => Ty::Data(*d, *m, args.clone(), vec![Row::ANY; rows.len()]),
        Ty::Array(elem, m) => Ty::Array(Box::new(erase_rows(elem)), *m),
        Ty::Fn(s, m) => Ty::Fn(Box::new(sig(s)), *m),
        Ty::Cont(s) => Ty::Cont(Box::new(sig(s))),
        Ty::Ref(inner) => Ty::Ref(Box::new(erase_rows(inner))),
        other => other.clone(),
    }
}

fn as_obj(value: &Value) -> Option<ObjId> {
    match value {
        Value::Obj(id) => Some(*id),
        _ => None,
    }
}

/// A `dup`, `drop` or other use of a freed object.
fn freed(what: &str) -> ErrorKind {
    ErrorKind::Undefined(format!("{what} of a freed object"))
}

/// Using a value that `undefined` made, or an array element that
/// `array_alloc` made and nothing set.
pub(crate) fn undefined_use() -> ErrorKind {
    ErrorKind::Undefined("use of an undefined value".into())
}
