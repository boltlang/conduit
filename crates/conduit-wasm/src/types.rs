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
//!
//! Mutability doesn't exist at run time, and every reference is nullable,
//! so locals of any type have a default value and join point parameters
//! can be set inside a `block` and read after it.

use std::collections::HashMap;

use conduit_ir::{CtorId, DataId, FuncId, Module, Sig, Ty};
use wasm_encoder::{
    ArrayType, CompositeInnerType, CompositeType, FieldType, FuncType, HeapType, RefType,
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
    Array(ValType),
    Cell(ValType),
    Boxed(ValType),
    /// The base type of closures with this signature.
    Closure(FnSig),
    /// The type of the wrapper of closures with this signature.
    Entry(FnSig),
    /// A closure of a function with this many captured values.
    Captures(FuncId, usize),
}

pub(crate) struct Types<'m> {
    m: &'m Module,
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
            Ty::I32 => ValType::I32,
            Ty::I64 => ValType::I64,
            Ty::F32 => ValType::F32,
            Ty::F64 => ValType::F64,
            Ty::Data(d, _) => nullable(self.data(*d)?),
            Ty::Array(elem, _) => nullable(self.array(elem)?),
            Ty::Fn(sig, _) => {
                let sig = self.sig(sig)?;
                nullable(self.closure(&sig)?)
            }
            Ty::Ref(inner) => nullable(self.cell(inner)?),
            Ty::Any(_) => ValType::Ref(RefType::ANYREF),
            Ty::Cont(_) => return Err("continuations aren't supported yet".into()),
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
                let fields = t.vals(&only.fields)?;
                let fields = fields.into_iter().map(|f| field(f, true)).collect();
                Ok(subtype(true, None, struct_(fields)))
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
            for f in t.vals(&decl.fields)? {
                fields.push(field(f, true));
            }
            Ok(subtype(true, Some(base), struct_(fields)))
        })?;
        Ok((idx, 1))
    }

    pub fn array(&mut self, elem: &Ty) -> Result<u32, String> {
        let elem = self.val(elem)?;
        self.get_or(Key::Array(elem), |_| {
            let inner = CompositeInnerType::Array(ArrayType(field(elem, true)));
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
}
