use std::collections::HashMap;
use std::rc::Rc;

use conduit_ir::Link;
use conduit_ir::types::subst_params;
use conduit_ir::{
    Atom, Block, ConstId, ConstValue, EffectId, Extern, ExternId, Func, FuncId, HandlerId, JoinDef,
    Label, Mutability, Op, Program, Row, Sig, SourceId, Tag, Tail, Ty, Var,
};

use crate::error::{Error, ErrorKind, HostError, unverified};
use crate::prim;
use crate::value::{Continuation, ObjId, Object, Value};

mod counts;
mod cycles;

pub use counts::{Count, Leaks, MAX_COUNT, Stats};
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
    /// The closures of the host functions the host holds, by number, until
    /// it lets go of them.
    functions: Vec<Option<Value>>,
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
    /// count them (see "Reference counts" in `docs/ir/ir.md`). Off, `dup` and
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
            functions: Vec::new(),
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
        // The caller is the host, so host values cross as at an extern
        // call: `Value::Host(n)` in, an object in the machine, and back out.
        let sig = self.p.func(func).map(Func::sig).unwrap_or_default();
        self.run_from_host(func, margs, Vec::new(), args, &sig.params, &sig.results)
    }

    /// Runs `func` for the host, with `captures` before `args`, which are
    /// the host's, of types `params`, converting host values both ways, as
    /// at an extern call.
    fn run_from_host(
        &mut self,
        func: FuncId,
        margs: Vec<Mutability>,
        captures: Vec<Value>,
        args: Vec<Value>,
        params: &[Ty],
        results: &[Ty],
    ) -> Result<Vec<Value>, Error> {
        let result = (|| {
            let mut all = captures;
            for (a, ty) in args.into_iter().zip(params) {
                let a = self
                    .take_from_host(a, ty)
                    .map_err(|what| unverified(format!("a {what} argument for a {ty}")))?;
                all.push(a);
            }
            self.enter(func, all, margs)?;
            let mut values = self.run()?;
            for (v, ty) in values.iter_mut().zip(results) {
                let given = self.give_to_host(*v, ty)?;
                if given != *v {
                    self.drop_value(*v)?;
                }
                *v = given;
            }
            Ok(values)
        })();
        result.map_err(|kind| {
            let error = self.error(kind);
            self.stack.clear();
            error
        })
    }

    /// What the host gave, of type `ty`, as the machine holds it: a host
    /// value or host function becomes an object. What doesn't fit, as a
    /// word for an error.
    fn take_from_host(&mut self, v: Value, ty: &Ty) -> Result<Value, &'static str> {
        Ok(match v {
            Value::Host(n) if ty.is_host() => self.alloc(Object::Host(n)),
            Value::Function(k) if ty.is_host() => self.alloc(Object::HostFunction(k)),
            Value::Null if *ty == Ty::NullableHost => v,
            Value::Host(_) | Value::Function(_) => return Err("host"),
            Value::Null => return Err("null"),
            _ if ty.is_host() => return Err("non-host"),
            _ => v,
        })
    }

    /// A value of type `ty` as the host sees it: a host value as its
    /// number, and a host function as its own. The object stays the
    /// machine's.
    fn give_to_host(&self, v: Value, ty: &Ty) -> Step<Value> {
        if !ty.is_host() || v == Value::Null {
            return Ok(v);
        }
        Ok(match self.object_ref(obj(v)?)? {
            Object::Host(n) => Value::Host(*n),
            Object::HostFunction(k) => Value::Function(*k),
            _ => return Err(unverified("a host value that isn't one")),
        })
    }

    /// Calls the closure of host function `k`, which a `host_function` gave
    /// the host, with `args`, as the host does when it calls the function:
    /// after the call that gave it is done, as from an event loop, since
    /// the machine doesn't run while it's calling the host.
    pub fn call_function(&mut self, k: u32, args: Vec<Value>) -> Result<Vec<Value>, Error> {
        if !self.stack.is_empty() {
            return Err(self.error(unverified("the machine is already running")));
        }
        let closure = match self.functions.get(k as usize) {
            Some(Some(c)) => *c,
            _ => return Err(self.error(unverified(format!("no host function {k}")))),
        };
        let (func, margs, captures) = self
            .closure_parts(closure, "a host function")
            .map_err(|kind| self.error(kind))?;
        let Some(sig) = self.p.func(func).map(Func::sig) else {
            return Err(self.error(unverified(format!("unknown function {func}"))));
        };
        let params = sig.params.get(captures.len()..).unwrap_or_default();
        // The host keeps the closure: the function owns its parameters, so
        // it gets references of its own to the captures, as `apply` gives.
        self.dup_all(&captures).map_err(|kind| self.error(kind))?;
        self.run_from_host(func, margs, captures, args, params, &sig.results)
    }

    /// Lets go of the closure of host function `k`, as the host does when
    /// it's done with the function.
    pub fn release_function(&mut self, k: u32) -> Result<(), Error> {
        let Some(closure) = self.functions.get_mut(k as usize).and_then(Option::take) else {
            return Err(self.error(unverified(format!("no host function {k}"))));
        };
        self.drop_value(closure).map_err(|kind| self.error(kind))
    }

    /// The closures of the host functions the host still holds.
    pub(crate) fn held_functions(&self) -> impl Iterator<Item = &Value> {
        self.functions.iter().flatten()
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
            ConstValue::Closure(f) => self.alloc_immortal(Object::Closure {
                func: *f,
                margs: Vec::new(),
                captures: Vec::new(),
                is_mut: false,
            }),
            ConstValue::Box(ty, inner) => {
                let value = self.const_value(inner)?;
                self.alloc_immortal(Object::Any {
                    ty: ty.clone(),
                    value,
                })
            }
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

    /// The function, mutability arguments and captures of closure `v`, for
    /// `what`, which runs it.
    fn closure_parts(&self, v: Value, what: &str) -> Step<(FuncId, Vec<Mutability>, Vec<Value>)> {
        match self.object_ref(obj(v)?)? {
            Object::Closure {
                func,
                margs,
                captures,
                ..
            } => Ok((*func, margs.clone(), captures.clone())),
            _ => Err(unverified(format!(
                "{what} of something that isn't a closure"
            ))),
        }
    }

    /// Takes closure `v` to run it, for `what`: its function, mutability
    /// arguments and captures, which the function owns, as its parameters.
    /// As if the closure gave up its captures, which it keeps if it's
    /// shared.
    fn open_closure(
        &mut self,
        v: Value,
        what: &str,
    ) -> Step<(FuncId, Vec<Mutability>, Vec<Value>)> {
        let parts = self.closure_parts(v, what)?;
        self.dup_all(&parts.2)?;
        self.drop_value(v)?;
        Ok(parts)
    }

    /// The elements of array `v`, for `what`, which reads them.
    fn array(&self, v: Value, what: &str) -> Step<&[Value]> {
        match self.object_ref(obj(v)?)? {
            Object::Array { elems, .. } => Ok(elems),
            _ => Err(unverified(format!(
                "{what} on something that isn't an array"
            ))),
        }
    }

    /// The elements of array `v`, for `what`, which writes them: an error
    /// unless the array is `Mut`.
    fn array_mut(&mut self, v: Value, what: &str) -> Step<&mut [Value]> {
        match self.object_mut(obj(v)?)? {
            Object::Array {
                elems,
                is_mut: true,
            } => Ok(elems),
            Object::Array { is_mut: false, .. } => Err(ErrorKind::WriteToImm),
            _ => Err(unverified(format!(
                "{what} on something that isn't an array"
            ))),
        }
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
        if is_tail(binds, next, &self.code()?.labels) {
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
            Op::CallExtern(e) => match self.call_extern(*e, args)? {
                Ok(results) => results,
                // What it threw is raised where it was called.
                Err(thrown) => return self.perform(EffectId::THROW, 0, vec![thrown], binds, next),
            },
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
                let (func, margs, mut all) = self.open_closure(*closure, "apply")?;
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
            Op::HostNull => vec![Value::Null],
            Op::HostIsNull => vec![Value::I32(i32::from(arg(&args, 0)? == Value::Null))],
            Op::HostUnwrap => match arg(&args, 0)? {
                Value::Null => return Err(ErrorKind::Trap),
                v => vec![v],
            },
            // The host holds the closure from now on.
            Op::HostFunction => {
                let closure = arg(&args, 0)?;
                let k = self.functions.len() as u32;
                self.functions.push(Some(closure));
                vec![self.alloc(Object::HostFunction(k))]
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
                        let old = std::mem::replace(field, value);
                        self.drop_value(defined(old, "set over")?)?;
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
                let (len, value) = (length(arg(&args, 0)?)?, arg(&args, 1)?);
                let is_mut = self.binds_mut(binds)?;
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
                let len = length(arg(&args, 0)?)?;
                vec![self.alloc(Object::Array {
                    elems: vec![Value::Unset; len],
                    is_mut: true,
                })]
            }
            Op::ArrayGet => {
                let index = index(arg(&args, 1)?)?;
                let elems = self.array(arg(&args, 0)?, "array get")?;
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
            Op::ArrayCopy => {
                let (dst, src) = (arg(&args, 0)?, arg(&args, 2)?);
                let (dst_start, src_start, count) = (
                    index(arg(&args, 1)?)?,
                    index(arg(&args, 3)?)?,
                    index(arg(&args, 4)?)?,
                );
                // The source's elements first, as through a temporary, so
                // that the two can be the same array.
                let from = self.array(src, "array copy")?;
                let end = src_start + count;
                let elems = (from.get(src_start..end))
                    .ok_or_else(|| out_of_bounds(end, from.len()))?
                    .to_vec();
                let to = self.array_mut(dst, "array copy")?;
                let (end, len) = (dst_start + count, to.len());
                let range = to
                    .get_mut(dst_start..end)
                    .ok_or_else(|| out_of_bounds(end, len))?;
                let old = range.to_vec();
                range.copy_from_slice(&elems);
                // What it copies first, so that copying an element over
                // itself doesn't free it.
                self.dup_all(&elems)?;
                old.into_iter().try_for_each(|v| self.drop_value(v))?;
                vec![]
            }
            Op::ArraySet => {
                let (index, value) = (index(arg(&args, 1)?)?, arg(&args, 2)?);
                let elems = self.array_mut(arg(&args, 0)?, "array set")?;
                let len = elems.len();
                let slot = elems
                    .get_mut(index)
                    .ok_or_else(|| out_of_bounds(index, len))?;
                let old = std::mem::replace(slot, value);
                self.drop_value(old)?;
                vec![]
            }
            Op::ArrayLoad => {
                let ty = self.bind_ty(binds)?;
                let at = index(arg(&args, 1)?)?;
                let size = scalar_size(&ty)?;
                let elems = self.array(arg(&args, 0)?, "array load")?;
                let end = at + size;
                let from = elems
                    .get(at..end)
                    .ok_or_else(|| out_of_bounds(end, elems.len()))?;
                let mut bytes = [0u8; 8];
                for (k, e) in from.iter().enumerate() {
                    bytes[k] = match *e {
                        Value::I8(b) => b as u8,
                        Value::Unset => {
                            return Err(ErrorKind::Undefined(format!(
                                "array load of byte {}, which isn't set",
                                at + k
                            )));
                        }
                        _ => return Err(unverified("array load from bytes that aren't i8")),
                    };
                }
                vec![from_le_bytes(&ty, bytes)?]
            }
            Op::ArrayStore => {
                let (at, value) = (index(arg(&args, 1)?)?, arg(&args, 2)?);
                let bytes = to_le_bytes(value)?;
                let elems = self.array_mut(arg(&args, 0)?, "array store")?;
                let (end, len) = (at + bytes.len(), elems.len());
                let to = elems
                    .get_mut(at..end)
                    .ok_or_else(|| out_of_bounds(end, len))?;
                for (e, &b) in to.iter_mut().zip(&bytes) {
                    *e = Value::I8(b as i8);
                }
                vec![]
            }
            Op::ArrayLen => vec![Value::I32(
                self.array(arg(&args, 0)?, "array len")?.len() as i32
            )],
            Op::ArrayClone => {
                let elems = self.array(arg(&args, 0)?, "array clone")?.to_vec();
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
                        self.drop_value(defined(old, "ref_set over")?)?;
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
                Object::Any { ty: boxed, value }
                    if conduit_ir::types::is_subtype(self.p, boxed, &self.concrete_ty(ty)?) =>
                {
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
                self.dup(defined(arg(&args, 0)?, "dup of")?)?;
                vec![]
            }
            Op::Drop => {
                self.drop_value(defined(arg(&args, 0)?, "drop of")?)?;
                vec![]
            }
            // The reference is given up without changing any count.
            Op::Forget => vec![],
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

    /// Calls the host's function for extern `e`. A host value is the
    /// host's number, both ways, and a null is itself. What it threw, if
    /// `e`'s row has `throw`, is the error, to raise.
    fn call_extern(
        &mut self,
        e: ExternId,
        mut args: Vec<Value>,
    ) -> Step<Result<Vec<Value>, Value>> {
        let decl = self
            .p
            .extern_fn(e)
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
        for (a, ty) in args.iter_mut().zip(&decl.sig.params) {
            *a = self.give_to_host(*a, ty)?;
        }
        let mut arrays = Arrays {
            heap: &mut self.heap,
        };
        let mut results = match (self.host).call(name, decl, &args, &mut arrays) {
            Ok(results) => results,
            Err(HostError::Throw(v)) if decl.sig.row.throws() => {
                let thrown = self
                    .take_from_host(v, &Ty::NullableHost)
                    .map_err(|what| ErrorKind::Host(format!("{name} threw a {what} value")))?;
                return Ok(Err(thrown));
            }
            Err(e) => return Err(ErrorKind::from(e)),
        };
        if results.len() != decl.sig.results.len() {
            return Err(ErrorKind::Host(format!(
                "{name} gave {} results instead of {}",
                results.len(),
                decl.sig.results.len()
            )));
        }
        for (r, ty) in results.iter_mut().zip(&decl.sig.results) {
            *r = self.take_from_host(*r, ty).map_err(|what| {
                ErrorKind::Host(format!(
                    "{name} gave a {what} result where its extern has a {ty}"
                ))
            })?;
        }
        Ok(Ok(results))
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
        let (func, margs, captures) = self.open_closure(closure, "handle")?;
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
        let Some(at) = self.stack.iter().rposition(handles) else {
            // An exception nothing catches goes to the host.
            if effect == EffectId::THROW
                && let [thrown] = &args[..]
            {
                let thrown = self.give_to_host(*thrown, &Ty::NullableHost)?;
                return Err(ErrorKind::Uncaught(crate::error::thrown(thrown)));
            }
            return Err(ErrorKind::Unhandled(effect));
        };
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
/// binds, straight to `ret` or through join points that only pass it on.
fn is_tail(binds: &[Var], next: At, labels: &[Option<&JoinDef>]) -> bool {
    next.index == next.block.lets.len()
        && conduit_ir::tail::returns(binds, &next.block.tail, labels)
}

fn arg(args: &[Value], i: usize) -> Step<Value> {
    args.get(i)
        .copied()
        .ok_or_else(|| unverified(format!("missing argument {i}")))
}

/// `value`, unless it's undefined, which what `what` says can't use.
fn defined(value: Value, what: &str) -> Step<Value> {
    match value {
        Value::Unset => Err(ErrorKind::Undefined(format!("{what} an undefined value"))),
        v => Ok(v),
    }
}

fn obj(value: Value) -> Step<ObjId> {
    match value {
        Value::Obj(id) => Ok(id),
        Value::Unset => Err(undefined_use()),
        _ => Err(unverified("a scalar where an object is expected")),
    }
}

/// The length of a new array. Lengths are unsigned, as in Wasm, but an
/// array is at most `i32::MAX` long, so that `ArrayLen` can give its
/// length.
fn length(value: Value) -> Step<usize> {
    match value {
        Value::I32(len) if len < 0 => Err(out_of_bounds(len as u32 as usize, i32::MAX as usize)),
        Value::I32(len) => Ok(len as usize),
        _ => Err(unverified("a length that isn't an i32")),
    }
}

fn index(value: Value) -> Step<usize> {
    match value {
        // Indices are unsigned, as in Wasm.
        Value::I32(i) => Ok(i as u32 as usize),
        _ => Err(unverified("an index that isn't an i32")),
    }
}

/// How many bytes `ArrayLoad` reads for a scalar of type `ty`.
fn scalar_size(ty: &Ty) -> Step<usize> {
    match ty {
        Ty::I8 => Ok(1),
        Ty::I16 => Ok(2),
        Ty::I32 | Ty::F32 => Ok(4),
        Ty::I64 | Ty::F64 => Ok(8),
        _ => Err(unverified("an array load of something that isn't a scalar")),
    }
}

/// The scalar of type `ty` whose little-endian bytes start `bytes`.
fn from_le_bytes(ty: &Ty, b: [u8; 8]) -> Step<Value> {
    Ok(match ty {
        Ty::I8 => Value::I8(b[0] as i8),
        Ty::I16 => Value::I16(i16::from_le_bytes([b[0], b[1]])),
        Ty::I32 => Value::I32(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        Ty::F32 => Value::F32(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        Ty::I64 => Value::I64(i64::from_le_bytes(b)),
        Ty::F64 => Value::F64(f64::from_le_bytes(b)),
        _ => return Err(unverified("an array load of something that isn't a scalar")),
    })
}

/// A scalar's bytes, little-endian, as `ArrayStore` writes them.
fn to_le_bytes(value: Value) -> Step<Vec<u8>> {
    Ok(match value {
        Value::I8(n) => vec![n as u8],
        Value::I16(n) => n.to_le_bytes().to_vec(),
        Value::I32(n) => n.to_le_bytes().to_vec(),
        Value::I64(n) => n.to_le_bytes().to_vec(),
        Value::F32(x) => x.to_le_bytes().to_vec(),
        Value::F64(x) => x.to_le_bytes().to_vec(),
        Value::Unset => return Err(undefined_use()),
        _ => {
            return Err(unverified(
                "an array store of something that isn't a scalar",
            ));
        }
    })
}

fn out_of_bounds(index: usize, len: usize) -> ErrorKind {
    ErrorKind::OutOfBounds {
        index: index as u64,
        len: len as u64,
    }
}

/// Finds every join definition of a function, without recursion.
fn join_defs(f: &Func) -> Labels<'_> {
    conduit_ir::tail::join_defs(&f.body, f.labels).into()
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
