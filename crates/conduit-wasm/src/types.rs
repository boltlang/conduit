//! How Conduit types become Wasm GC types.
//!
//! Every struct, array and closure type lives in one recursive type group,
//! so ADTs can refer to each other in any order, and two Conduit types with
//! the same shape still get distinct Wasm types. Types are added to the
//! group the first time they're needed. A key is reserved before its
//! contents are built, so a type that refers to itself finds its own index.
//!
//! The layouts:
//!
//! - An ADT with one constructor is a struct of its fields.
//! - An ADT with several constructors has a base struct holding an `i32`
//!   tag, and each constructor is a subtype of it with the tag first and
//!   then the fields. `case` reads the tag and `Get` casts to the
//!   constructor.
//! - A closure of signature `s` is a subtype of a base struct holding a
//!   reference to its wrapper (`entry`), whose type is
//!   `func (env, s.params..) -> s.results`. The concrete closure of a
//!   function adds the captured values after it.
//! - A ref is a struct with one mutable field, and a boxed scalar is a
//!   struct with one immutable field.
//! - `Any` is `anyref`.
//! - A handler frame, which `handle` pushes on the handler stack, is a
//!   subtype of a base holding its effect's number and the next frame. A
//!   frame for an effect adds a reference to each of its clauses, and a
//!   handler's frame adds its environment, then, if it has an aborting
//!   clause, a mutable field for each result of the `handle`, which that
//!   clause sets before it unwinds. A clause is called with the frame, then
//!   the operation's parameters.
//! - A handler that switches stacks pushes a frame of its own type, which
//!   adds nothing to the base. Its `next` is then mutable, in every frame,
//!   since it's the handlers around wherever its continuation was last
//!   resumed. A continuation is a closure of its signature, whose concrete
//!   type holds the frame, the handler's environment and the Wasm
//!   continuation. If some clause discontinues one, its base also holds
//!   the function that does that.
//!
//! Mutability doesn't exist at run time, and every reference is nullable,
//! so locals of any type have a default value and join point parameters
//! can be set inside a `block` and read after it.

use std::collections::{BTreeSet, HashMap};

use conduit_ir::{CtorId, DataId, EffectId, FuncId, HandlerId, Module, Sig, Ty};
use wasm_encoder::{
    ArrayType, CompositeInnerType, CompositeType, ContType, FieldType, FuncType, HeapType, RefType,
    StorageType, StructType, SubType, ValType,
};

/// A signature after lowering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FnSig {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    /// An ADT: its only constructor, or the base of its constructors.
    Data(DataId),
    /// A constructor of an ADT with several of them.
    Ctor(CtorId),
    Array(StorageType),
    Cell(ValType),
    Boxed(ValType),
    /// The base type of closures with this signature.
    Closure(FnSig),
    /// The type of the wrapper of closures with this signature.
    Entry(FnSig),
    /// A closure of a function with this many captured values.
    Captures(FuncId, usize),
    /// The base type of handler frames.
    Frame,
    /// The frames of handlers of an effect.
    EffectFrame(EffectId),
    /// The type of the clauses for operation `i` of an effect.
    Clause(EffectId, u32),
    /// The frames of a handler.
    HandlerFrame(HandlerId),
    /// A function type, as a block type or a continuation's.
    Func(FnSig),
    /// A Wasm continuation of a function type.
    Cont(FnSig),
    /// A Wasm continuation of the wrapper of closures with this signature.
    ContEntry(FnSig),
    /// The base type of Conduit continuations with this signature.
    ContBase(FnSig),
    /// The type of the function that discontinues a continuation with
    /// this signature: `func (k, thunk) -> results`.
    Discontinue(FnSig),
    /// The frames of handlers that switch stacks.
    SwitchFrame,
    /// The continuation a handler's clause for operation `i` gets.
    ContOf(HandlerId, u32),
    /// What `discontinue E.i(k, args)` throws into `k`: a closure that
    /// performs `E.i(args)`.
    Thunk(EffectId, u32),
}

pub(crate) struct Types<'m> {
    m: &'m Module,
    /// The handlers with an aborting clause.
    pub aborting: BTreeSet<HandlerId>,
    /// The handlers that switch stacks, since a clause of theirs is
    /// neither tail-resumptive nor aborting.
    pub switching: BTreeSet<HandlerId>,
    /// Whether some clause discontinues a continuation it was given.
    pub discontinues: bool,
    group: Vec<Option<SubType>>,
    keys: HashMap<Key, u32>,
}

/// A nullable reference to a type of the group.
pub(crate) fn nullable(idx: u32) -> ValType {
    ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(idx),
    })
}

fn field(ty: ValType, mutable: bool) -> FieldType {
    FieldType {
        element_type: StorageType::Val(ty),
        mutable,
    }
}

/// A field of a data type, of type `ty`: packed for `i8` and `i16`, as an
/// array's elements are, and mutable, as every field is.
fn data_field(types: &mut Types<'_>, ty: &Ty) -> Result<FieldType, String> {
    Ok(FieldType {
        element_type: storage(types, ty)?,
        mutable: true,
    })
}

/// How an array stores elements of type `ty`: `i8` and `i16` packed.
fn storage(types: &mut Types<'_>, ty: &Ty) -> Result<StorageType, String> {
    Ok(match ty {
        Ty::I8 => StorageType::I8,
        Ty::I16 => StorageType::I16,
        ty => StorageType::Val(types.val(ty)?),
    })
}

fn subtype(is_final: bool, supertype: Option<u32>, inner: CompositeInnerType) -> SubType {
    SubType {
        is_final,
        supertype_idxs: supertype.into_iter().collect(),
        composite_type: CompositeType {
            inner,
            shared: false,
            descriptor: None,
            describes: None,
        },
    }
}

fn struct_(fields: Vec<FieldType>) -> CompositeInnerType {
    CompositeInnerType::Struct(StructType {
        fields: fields.into(),
    })
}

impl<'m> Types<'m> {
    pub fn new(m: &'m Module) -> Self {
        Types {
            m,
            aborting: BTreeSet::new(),
            switching: BTreeSet::new(),
            discontinues: false,
            group: Vec::new(),
            keys: HashMap::new(),
        }
    }

    /// The finished group. Every reserved type must have been built.
    pub fn finish(self) -> Vec<SubType> {
        self.group
            .into_iter()
            .map(|t| t.expect("a type of the group was reserved but never built"))
            .collect()
    }

    fn get_or(
        &mut self,
        key: Key,
        build: impl FnOnce(&mut Self) -> Result<SubType, String>,
    ) -> Result<u32, String> {
        if let Some(&idx) = self.keys.get(&key) {
            return Ok(idx);
        }
        let idx = self.group.len() as u32;
        self.group.push(None);
        self.keys.insert(key, idx);
        self.group[idx as usize] = Some(build(self)?);
        Ok(idx)
    }

    pub fn val(&mut self, ty: &Ty) -> Result<ValType, String> {
        Ok(match ty {
            // An `i8` or `i16` is an `i32` whose low bits are the value. The
            // high bits can be anything: see `func.rs`.
            Ty::I8 | Ty::I16 | Ty::I32 => ValType::I32,
            Ty::I64 => ValType::I64,
            Ty::F32 => ValType::F32,
            Ty::F64 => ValType::F64,
            Ty::Data(d, ..) => nullable(self.data(*d)?),
            Ty::Array(elem, _) => nullable(self.array(elem)?),
            Ty::Fn(sig, _) => {
                let sig = self.sig(sig)?;
                nullable(self.closure(&sig)?)
            }
            Ty::Ref(inner) => nullable(self.cell(inner)?),
            Ty::Any(_) => ValType::Ref(RefType::ANYREF),
            Ty::Cont(sig) => {
                let sig = self.sig(sig)?;
                nullable(self.cont_base(&sig)?)
            }
        })
    }

    pub fn vals(&mut self, tys: &[Ty]) -> Result<Vec<ValType>, String> {
        tys.iter().map(|t| self.val(t)).collect()
    }

    pub fn sig(&mut self, sig: &Sig) -> Result<FnSig, String> {
        Ok(FnSig {
            params: self.vals(&sig.params)?,
            results: self.vals(&sig.results)?,
        })
    }

    fn ctors(&self, d: DataId) -> Result<&'m [conduit_ir::CtorDecl], String> {
        let m: &'m Module = self.m;
        match m.data.get(&d) {
            Some(decl) => Ok(&decl.ctors),
            None => Err(format!("unknown data type {d}")),
        }
    }

    /// Whether values of `d` carry a tag, because it has several
    /// constructors (or none).
    pub fn tagged(&self, d: DataId) -> Result<bool, String> {
        Ok(self.ctors(d)?.len() != 1)
    }

    /// The type of values of `d`.
    pub fn data(&mut self, d: DataId) -> Result<u32, String> {
        let ctors = self.ctors(d)?;
        if let [only] = ctors {
            self.get_or(Key::Data(d), |t| {
                let fields = only.fields.iter().map(|f| data_field(t, f));
                Ok(subtype(
                    true,
                    None,
                    struct_(fields.collect::<Result<_, _>>()?),
                ))
            })
        } else {
            self.get_or(Key::Data(d), |_| {
                Ok(subtype(
                    false,
                    None,
                    struct_(vec![field(ValType::I32, false)]),
                ))
            })
        }
    }

    /// The struct holding the fields of `c`, and the index of its first
    /// field (1 after a tag, else 0).
    pub fn ctor(&mut self, c: CtorId) -> Result<(u32, u32), String> {
        let base = self.data(c.data)?;
        if !self.tagged(c.data)? {
            return Ok((base, 0));
        }
        let Some(decl) = self.ctors(c.data)?.get(c.index as usize) else {
            return Err(format!("unknown constructor {c}"));
        };
        let idx = self.get_or(Key::Ctor(c), |t| {
            let mut fields = vec![field(ValType::I32, false)];
            for f in &decl.fields {
                fields.push(data_field(t, f)?);
            }
            Ok(subtype(true, Some(base), struct_(fields)))
        })?;
        Ok((idx, 1))
    }

    pub fn array(&mut self, elem: &Ty) -> Result<u32, String> {
        let elem = storage(self, elem)?;
        self.get_or(Key::Array(elem), |_| {
            let inner = CompositeInnerType::Array(ArrayType(FieldType {
                element_type: elem,
                mutable: true,
            }));
            Ok(subtype(true, None, inner))
        })
    }

    pub fn cell(&mut self, inner: &Ty) -> Result<u32, String> {
        let inner = self.val(inner)?;
        self.get_or(Key::Cell(inner), |_| {
            Ok(subtype(true, None, struct_(vec![field(inner, true)])))
        })
    }

    /// The box of a scalar.
    pub fn boxed(&mut self, scalar: ValType) -> Result<u32, String> {
        self.get_or(Key::Boxed(scalar), |_| {
            Ok(subtype(true, None, struct_(vec![field(scalar, false)])))
        })
    }

    pub fn closure(&mut self, sig: &FnSig) -> Result<u32, String> {
        self.get_or(Key::Closure(sig.clone()), |t| {
            let entry = t.entry(sig)?;
            Ok(subtype(
                false,
                None,
                struct_(vec![field(nullable(entry), false)]),
            ))
        })
    }

    pub fn entry(&mut self, sig: &FnSig) -> Result<u32, String> {
        self.get_or(Key::Entry(sig.clone()), |t| {
            let env = nullable(t.closure(sig)?);
            let params = std::iter::once(env).chain(sig.params.iter().copied());
            let ty = FuncType::new(params, sig.results.iter().copied());
            Ok(subtype(true, None, CompositeInnerType::Func(ty)))
        })
    }

    /// The closure of `f` with its first `n` parameters captured: the type
    /// of the closure object, and the signature it's called with.
    pub fn captures(&mut self, f: FuncId, n: usize) -> Result<(u32, FnSig), String> {
        let m: &'m Module = self.m;
        let Some(func) = m.funcs.get(&f) else {
            return Err(format!("unknown function {f}"));
        };
        let sig = func.sig();
        if n > sig.params.len() {
            return Err(format!(
                "closure of {f} captures more values than it has parameters"
            ));
        }
        let captured = self.vals(&sig.params[..n])?;
        let rest = FnSig {
            params: self.vals(&sig.params[n..])?,
            results: self.vals(&sig.results)?,
        };
        let base = self.closure(&rest)?;
        let entry = self.entry(&rest)?;
        let idx = self.get_or(Key::Captures(f, n), |_| {
            let mut fields = vec![field(nullable(entry), false)];
            fields.extend(captured.into_iter().map(|c| field(c, false)));
            Ok(subtype(true, Some(base), struct_(fields)))
        })?;
        Ok((idx, rest))
    }

    /// The base type of handler frames: an effect's number and the next
    /// frame.
    pub fn frame(&mut self) -> Result<u32, String> {
        self.get_or(Key::Frame, |t| {
            let next = nullable(t.keys[&Key::Frame]);
            let mutable = !t.switching.is_empty();
            Ok(subtype(
                false,
                None,
                struct_(vec![field(ValType::I32, false), field(next, mutable)]),
            ))
        })
    }

    /// The fields of a frame for `e`: the base's, then a clause per
    /// operation.
    fn effect_fields(&mut self, e: EffectId) -> Result<Vec<FieldType>, String> {
        let next = nullable(self.frame()?);
        let ops = self.ops(e)?;
        let mutable = !self.switching.is_empty();
        let mut fields = vec![field(ValType::I32, false), field(next, mutable)];
        for i in 0..ops {
            fields.push(field(nullable(self.clause(e, i)?), false));
        }
        Ok(fields)
    }

    fn ops(&self, e: EffectId) -> Result<u32, String> {
        match self.m.effects.get(&e) {
            Some(decl) => Ok(decl.ops.len() as u32),
            None => Err(format!("unknown effect {e}")),
        }
    }

    /// The frames of handlers of `e`.
    pub fn effect_frame(&mut self, e: EffectId) -> Result<u32, String> {
        let base = self.frame()?;
        self.get_or(Key::EffectFrame(e), |t| {
            Ok(subtype(false, Some(base), struct_(t.effect_fields(e)?)))
        })
    }

    /// The type of the clauses for operation `i` of `e`:
    /// `func (frame, op params..) -> op results`.
    pub fn clause(&mut self, e: EffectId, i: u32) -> Result<u32, String> {
        let m: &'m Module = self.m;
        let Some(op) = m.effects.get(&e).and_then(|d| d.ops.get(i as usize)) else {
            return Err(format!("{e} has no operation {i}"));
        };
        self.get_or(Key::Clause(e, i), |t| {
            let frame = nullable(t.effect_frame(e)?);
            let sig = t.sig(op)?;
            let params = std::iter::once(frame).chain(sig.params);
            let ty = FuncType::new(params, sig.results);
            Ok(subtype(true, None, CompositeInnerType::Func(ty)))
        })
    }

    /// The frames of `h`: those of its effect, its environment, and its
    /// results if it has an aborting clause. Also gives the index of the
    /// first field of the environment.
    pub fn handler_frame(&mut self, h: HandlerId) -> Result<(u32, u32), String> {
        let m: &'m Module = self.m;
        let Some(decl) = m.handlers.get(&h) else {
            return Err(format!("unknown handler {h}"));
        };
        let base = self.effect_frame(decl.effect)?;
        let first = 2 + self.ops(decl.effect)?;
        let idx = self.get_or(Key::HandlerFrame(h), |t| {
            let mut fields = t.effect_fields(decl.effect)?;
            for ty in t.vals(&decl.env)? {
                fields.push(field(ty, false));
            }
            if t.aborting.contains(&h) {
                let Some(ret) = m.funcs.get(&decl.ret) else {
                    return Err(format!("unknown return clause {}", decl.ret));
                };
                for ty in t.vals(&ret.results)? {
                    fields.push(field(ty, true));
                }
            }
            Ok(subtype(true, Some(base), struct_(fields)))
        })?;
        Ok((idx, first))
    }

    /// A function type in the group.
    pub fn func(&mut self, sig: &FnSig) -> Result<u32, String> {
        self.get_or(Key::Func(sig.clone()), |_| {
            let ty = FuncType::new(sig.params.iter().copied(), sig.results.iter().copied());
            Ok(subtype(true, None, CompositeInnerType::Func(ty)))
        })
    }

    /// A Wasm continuation that's resumed with `sig.params` and gives
    /// `sig.results`.
    pub fn cont(&mut self, sig: &FnSig) -> Result<u32, String> {
        self.get_or(Key::Cont(sig.clone()), |t| {
            let f = t.func(sig)?;
            Ok(subtype(true, None, CompositeInnerType::Cont(ContType(f))))
        })
    }

    /// A Wasm continuation of a closure's wrapper, which is resumed with
    /// the closure.
    pub fn cont_of_entry(&mut self, sig: &FnSig) -> Result<u32, String> {
        let entry = self.entry(sig)?;
        self.get_or(Key::ContEntry(sig.clone()), |_| {
            Ok(subtype(
                true,
                None,
                CompositeInnerType::Cont(ContType(entry)),
            ))
        })
    }

    /// The base type of Conduit continuations with signature `sig`: a
    /// closure's, and if some clause discontinues one, the function that
    /// does it.
    pub fn cont_base(&mut self, sig: &FnSig) -> Result<u32, String> {
        let closure = self.closure(sig)?;
        if !self.discontinues {
            return Ok(closure);
        }
        let entry = self.entry(sig)?;
        self.get_or(Key::ContBase(sig.clone()), |t| {
            let disc = t.discontinue(sig)?;
            Ok(subtype(
                false,
                Some(closure),
                struct_(vec![
                    field(nullable(entry), false),
                    field(nullable(disc), false),
                ]),
            ))
        })
    }

    /// `func (k, thunk) -> sig.results`, which discontinues `k` by
    /// throwing the closure `thunk` into it.
    pub fn discontinue(&mut self, sig: &FnSig) -> Result<u32, String> {
        self.get_or(Key::Discontinue(sig.clone()), |t| {
            let k = nullable(t.cont_base(sig)?);
            let ty = FuncType::new(
                [k, ValType::Ref(RefType::ANYREF)],
                sig.results.iter().copied(),
            );
            Ok(subtype(true, None, CompositeInnerType::Func(ty)))
        })
    }

    /// The frames of handlers that switch stacks.
    pub fn switch_frame(&mut self) -> Result<u32, String> {
        let base = self.frame()?;
        self.get_or(Key::SwitchFrame, |t| {
            let next = nullable(t.frame()?);
            Ok(subtype(
                true,
                Some(base),
                struct_(vec![field(ValType::I32, false), field(next, true)]),
            ))
        })
    }

    /// The results of the body of a `handle` of `h`: what its return
    /// clause takes after the environment.
    pub fn body_results(&mut self, h: HandlerId) -> Result<Vec<ValType>, String> {
        let m: &'m Module = self.m;
        let decl = &m.handlers[&h];
        let Some(ret) = m.funcs.get(&decl.ret) else {
            return Err(format!("unknown return clause {}", decl.ret));
        };
        let sig = ret.sig();
        let Some(body) = sig.params.get(decl.env.len()..) else {
            return Err(format!(
                "return clause {} doesn't take the environment",
                decl.ret
            ));
        };
        self.vals(body)
    }

    /// The Wasm continuation captured by operation `i` under `h`: resumed
    /// with the operation's results, it gives the body's.
    pub fn op_cont(&mut self, h: HandlerId, i: u32) -> Result<u32, String> {
        let m: &'m Module = self.m;
        let decl = &m.handlers[&h];
        let Some(op) = m
            .effects
            .get(&decl.effect)
            .and_then(|e| e.ops.get(i as usize))
        else {
            return Err(format!("{} has no operation {i}", decl.effect));
        };
        let sig = FnSig {
            params: self.vals(&op.results)?,
            results: self.body_results(h)?,
        };
        self.cont(&sig)
    }

    /// The signature of the continuation clause `i` of `h` gets: the
    /// operation's results to what the `handle` gives.
    pub fn clause_cont_sig(&mut self, h: HandlerId, i: u32) -> Result<FnSig, String> {
        let m: &'m Module = self.m;
        let decl = &m.handlers[&h];
        let Some(op) = m
            .effects
            .get(&decl.effect)
            .and_then(|e| e.ops.get(i as usize))
        else {
            return Err(format!("{} has no operation {i}", decl.effect));
        };
        let Some(ret) = m.funcs.get(&decl.ret) else {
            return Err(format!("unknown return clause {}", decl.ret));
        };
        Ok(FnSig {
            params: self.vals(&op.results)?,
            results: self.vals(&ret.results)?,
        })
    }

    /// The continuation clause `i` of `h` gets, and the index of its
    /// first field after the base's: the frame, then the environment, then
    /// the Wasm continuation.
    pub fn cont_of(&mut self, h: HandlerId, i: u32) -> Result<(u32, u32), String> {
        let m: &'m Module = self.m;
        let decl = &m.handlers[&h];
        let sig = self.clause_cont_sig(h, i)?;
        let base = self.cont_base(&sig)?;
        let first = if self.discontinues { 2 } else { 1 };
        let idx = self.get_or(Key::ContOf(h, i), |t| {
            let entry = t.entry(&sig)?;
            let mut fields = vec![field(nullable(entry), false)];
            if t.discontinues {
                fields.push(field(nullable(t.discontinue(&sig)?), false));
            }
            fields.push(field(nullable(t.switch_frame()?), false));
            for ty in t.vals(&decl.env)? {
                fields.push(field(ty, false));
            }
            fields.push(field(nullable(t.op_cont(h, i)?), false));
            Ok(subtype(true, Some(base), struct_(fields)))
        })?;
        Ok((idx, first))
    }

    /// What `discontinue E.i(k, args)` throws into `k`: a closure of no
    /// parameters that performs `E.i(args)`, holding `args`. Also gives
    /// the signature it's called with.
    pub fn thunk(&mut self, e: EffectId, i: u32) -> Result<(u32, FnSig), String> {
        let m: &'m Module = self.m;
        let Some(op) = m.effects.get(&e).and_then(|d| d.ops.get(i as usize)) else {
            return Err(format!("{e} has no operation {i}"));
        };
        let sig = FnSig {
            params: Vec::new(),
            results: self.vals(&op.results)?,
        };
        let base = self.closure(&sig)?;
        let entry = self.entry(&sig)?;
        let args = self.vals(&op.params)?;
        let idx = self.get_or(Key::Thunk(e, i), |_| {
            let mut fields = vec![field(nullable(entry), false)];
            fields.extend(args.into_iter().map(|a| field(a, false)));
            Ok(subtype(true, Some(base), struct_(fields)))
        })?;
        Ok((idx, sig))
    }
}
