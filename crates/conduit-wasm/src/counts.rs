//! Reference counting in linear memory, for a module whose counts are
//! explicit (see "Reference counts" in `docs/ir.md`), when
//! [`Options::counts`](crate::Options::counts) asks for it.
//!
//! - **The header word** is the count. A constant's has its sign bit set,
//!   so counting skips it: it's immortal.
//! - **Memory** comes back through `release`, onto a free list per size,
//!   one for each multiple of 8 up to 256 bytes and one for bigger blocks,
//!   whose heads are a table in memory after the constants. `alloc` takes
//!   a block from its list, of the same size for a big one, before it
//!   bumps the heap, zeroes it, and sets its count to 1.
//! - **Dropping** goes through a function for each kind of object, a
//!   [`DropKey`]: a data type, arrays and refs of what they hold,
//!   closures, and `any`. At 0, it frees the object and drops what it
//!   held, tail-calling the drop of its last field, so that dropping a
//!   list is a loop. A closure's own function frees it, found in the table
//!   from its wrapper's slot, and an `any` is always a box, which holds the
//!   kind of what it holds.
//!
//! The functions come right after `alloc`, in the order of the methods
//! below, so their indices are known before any code is lowered: the
//! kinds are found up front, from every type in the module.

use std::collections::BTreeMap;

use conduit_ir::{Block, DataId, Module, Op, Tail, Ty};
use wasm_encoder::{BlockType, Instruction as I, ValType};

use crate::func::{Body, load, store};
use crate::{Cx, Storage, layout};

/// The global counting live objects, which tests compare with the
/// evaluator's.
pub(crate) const LIVE: u32 = 3;

/// Size classes have free lists up to this many 8-byte words.
const CLASSES: u32 = 32;

/// The functions every counted module has, before those of each kind.
const FIXED: u32 = 6;

/// The functions of each kind.
const PER_KIND: u32 = 4;

/// A kind of object that's dropped the same way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DropKey {
    Data(DataId),
    /// What its elements are stored as, and how they're dropped.
    Array(Storage, Option<Box<DropKey>>),
    Ref(Storage, Option<Box<DropKey>>),
    Closure,
    Any,
}

/// How a value of type `ty` is dropped, or `None` if it isn't counted.
/// Continuations aren't: what they hold is lost when they're dropped.
pub(crate) fn key_of(ty: &Ty) -> Option<DropKey> {
    Some(match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 | Ty::Cont(_) | Ty::Reuse(_) => {
            return None;
        }
        Ty::Data(d, ..) => DropKey::Data(*d),
        Ty::Array(e, _) => DropKey::Array(Storage::of(e), key_of(e).map(Box::new)),
        Ty::Ref(t) => DropKey::Ref(Storage::of(t), key_of(t).map(Box::new)),
        Ty::Fn(..) => DropKey::Closure,
        Ty::Any(_) => DropKey::Any,
    })
}

/// The functions of reference counting, and where things are.
pub(crate) struct Runtime {
    /// The index of the first function.
    base: u32,
    pub keys: Vec<DropKey>,
    index: BTreeMap<DropKey, u32>,
    /// The address of the free lists' heads: the big blocks', then one per
    /// size class from 8 bytes.
    pub heads: u32,
}

impl Runtime {
    /// Finds the kinds of `m`'s objects, for functions from `base`, with
    /// the heads of the free lists at `heads`.
    pub fn new(m: &Module, base: u32, heads: u32) -> Runtime {
        let mut rt = Runtime {
            base,
            keys: Vec::new(),
            index: BTreeMap::new(),
            heads,
        };
        let mut tys: Vec<&Ty> = Vec::new();
        for f in m.funcs.values() {
            tys.extend(&f.vars);
            tys.extend(&f.results);
            let mut todo = vec![&f.body];
            while let Some(b) = todo.pop() {
                for l in &b.lets {
                    if let Op::Box(t) | Op::Unbox(t) = &l.op {
                        tys.push(t);
                    }
                }
                children(b, &mut todo);
            }
        }
        for d in m.data.values() {
            tys.extend(d.ctors.iter().flat_map(|c| &c.fields));
        }
        for h in m.handlers.values() {
            tys.extend(&h.env);
        }
        tys.extend(m.consts.values().map(|c| &c.ty));
        let mut todo: Vec<DropKey> = tys.into_iter().filter_map(key_of).collect();
        while let Some(k) = todo.pop() {
            if rt.index.contains_key(&k) {
                continue;
            }
            rt.index.insert(k.clone(), rt.keys.len() as u32);
            match &k {
                DropKey::Data(d) => {
                    let fields = m.data.get(d).into_iter().flat_map(|d| &d.ctors);
                    todo.extend(fields.flat_map(|c| &c.fields).filter_map(key_of));
                }
                DropKey::Array(_, Some(e)) | DropKey::Ref(_, Some(e)) => todo.push((**e).clone()),
                _ => {}
            }
            rt.keys.push(k);
        }
        rt
    }

    /// How many functions it takes.
    pub fn len(&self) -> u32 {
        FIXED + PER_KIND * self.keys.len() as u32
    }

    /// `dup(p)`.
    pub fn dup(&self) -> u32 {
        self.base
    }

    /// `add(p, n)`: adds `n` to the count.
    pub fn add(&self) -> u32 {
        self.base + 1
    }

    /// `decref(p)`.
    pub fn decref(&self) -> u32 {
        self.base + 2
    }

    /// `is_unique(p) -> i32`.
    pub fn is_unique(&self) -> u32 {
        self.base + 3
    }

    /// `release(p, size)`: gives the memory back.
    pub fn release(&self) -> u32 {
        self.base + 4
    }

    /// `dup_range(address, n)`: dups `n` references stored from `address`.
    pub fn dup_range(&self) -> u32 {
        self.base + 5
    }

    /// The number of a kind. A box holds it plus 1.
    pub fn number(&self, k: &DropKey) -> u32 {
        self.index[k]
    }

    /// `drop(p)` of a kind.
    pub fn drop(&self, k: &DropKey) -> u32 {
        self.base + FIXED + PER_KIND * self.number(k)
    }

    /// `drop_range(address, n)`: drops `n` references of a kind stored
    /// from `address`.
    pub fn drop_range(&self, k: &DropKey) -> u32 {
        self.drop(k) + 1
    }

    /// `free(p)` of a kind: gives its memory back without dropping what it
    /// holds.
    pub fn free(&self, k: &DropKey) -> u32 {
        self.drop(k) + 2
    }

    /// `drop_reuse(p) -> token` of a data type: like `drop`, but at 0 it
    /// keeps the memory, for the token, which is otherwise 0.
    pub fn drop_reuse(&self, k: &DropKey) -> u32 {
        self.drop(k) + 3
    }

    /// The bodies of the functions, with their types. A closure's free
    /// function is at its wrapper's slot plus `closures`, and a kind's drop
    /// at its number plus `kinds`.
    pub fn bodies(&self, cx: &mut Cx<'_>, closures: i32, kinds: i32) -> Vec<(u32, Body)> {
        let one = cx.sig(sig(&[ValType::I32], &[]));
        let two = cx.sig(sig(&[ValType::I32, ValType::I32], &[]));
        let test = cx.sig(sig(&[ValType::I32], &[ValType::I32]));
        let mut out = vec![
            (one, self.change(Change::Dup)),
            (two, self.change(Change::Add)),
            (one, self.change(Change::DecRef)),
            (test, is_unique()),
            (two, self.release_body()),
            (two, range(self.dup())),
        ];
        for k in &self.keys {
            out.push((one, self.drop_body(cx, k, closures, kinds, one)));
            out.push((two, range(self.drop(k))));
            out.push((one, self.free_body(cx, k)));
            out.push((test, self.drop_reuse_body(cx, k)));
        }
        out
    }

    /// `dup`, `add` and `decref`: change the count, unless the object is 0
    /// (undefined) or immortal.
    fn change(&self, change: Change) -> Body {
        let (p, h) = (0, if change == Change::Add { 2 } else { 1 });
        let mut code = skip_uncounted(p, h);
        code.extend([I::LocalGet(p), I::LocalGet(h)]);
        match change {
            Change::Dup => code.extend([I::I32Const(1), I::I32Add]),
            Change::Add => code.extend([I::LocalGet(1), I::I32Add]),
            Change::DecRef => code.extend([I::I32Const(1), I::I32Sub]),
        }
        code.extend([store(Storage::I32, 0), I::End]);
        Body {
            locals: vec![ValType::I32],
            code,
        }
    }

    /// `release(p, size)`: puts the block on its free list.
    fn release_body(&self) -> Body {
        let (p, size, class, head) = (0, 1, 2, 3);
        let heads = self.heads as i32;
        let code = vec![
            I::LocalGet(size),
            I::I32Const(7),
            I::I32Add,
            I::I32Const(-8),
            I::I32And,
            I::LocalTee(size),
            I::I32Const(3),
            I::I32ShrU,
            I::LocalTee(class),
            I::I32Const(CLASSES as i32),
            I::I32GtU,
            I::If(BlockType::Empty),
            // A big block: on the list of big blocks, with its size.
            I::LocalGet(p),
            I::LocalGet(size),
            store(Storage::I32, 4),
            I::I32Const(heads),
            I::LocalSet(head),
            I::Else,
            I::LocalGet(class),
            I::I32Const(2),
            I::I32Shl,
            I::I32Const(heads),
            I::I32Add,
            I::LocalSet(head),
            I::End,
            I::LocalGet(p),
            I::LocalGet(head),
            load(Storage::I32, 0),
            store(Storage::I32, 0),
            I::LocalGet(head),
            I::LocalGet(p),
            store(Storage::I32, 0),
            I::GlobalGet(LIVE),
            I::I32Const(1),
            I::I32Sub,
            I::GlobalSet(LIVE),
            I::End,
        ];
        Body {
            locals: vec![ValType::I32, ValType::I32],
            code,
        }
    }

    /// `drop(p)` of kind `k`.
    fn drop_body(&self, cx: &mut Cx<'_>, k: &DropKey, closures: i32, kinds: i32, one: u32) -> Body {
        let (p, h) = (0, 1);
        let mut code = skip_uncounted(p, h);
        // Shared: one less.
        code.extend([
            I::LocalGet(h),
            I::I32Const(1),
            I::I32Ne,
            I::If(BlockType::Empty),
            I::LocalGet(p),
            I::LocalGet(h),
            I::I32Const(1),
            I::I32Sub,
            store(Storage::I32, 0),
            I::Return,
            I::End,
        ]);
        let mut locals = vec![ValType::I32];
        match k {
            DropKey::Data(d) => {
                let ctor = 2;
                locals.push(ValType::I32);
                code.extend([I::LocalGet(p), load(Storage::I32, 4), I::LocalSet(ctor)]);
                let ctors = cx.m.data.get(d).map_or(&[][..], |d| &d.ctors[..]);
                for (i, c) in ctors.iter().enumerate() {
                    let (offsets, size) = layout(&c.fields);
                    let held: Vec<(u32, DropKey)> = c
                        .fields
                        .iter()
                        .zip(&offsets)
                        .filter_map(|(t, &(at, _))| key_of(t).map(|k| (at, k)))
                        .collect();
                    code.extend([
                        I::LocalGet(ctor),
                        I::I32Const(i as i32),
                        I::I32Eq,
                        I::If(BlockType::Empty),
                    ]);
                    // What it holds first, before its memory is reused.
                    let first = locals.len() as u32 + 1;
                    for (j, (at, _)) in held.iter().enumerate() {
                        locals.push(ValType::I32);
                        code.extend([
                            I::LocalGet(p),
                            load(Storage::I32, *at),
                            I::LocalSet(first + j as u32),
                        ]);
                    }
                    code.extend([
                        I::LocalGet(p),
                        I::I32Const(size as i32),
                        I::Call(self.release()),
                    ]);
                    for (j, (_, k)) in held.iter().enumerate() {
                        code.push(I::LocalGet(first + j as u32));
                        code.push(if j + 1 == held.len() {
                            I::ReturnCall(self.drop(k))
                        } else {
                            I::Call(self.drop(k))
                        });
                    }
                    code.extend([I::Return, I::End]);
                }
                code.push(I::Unreachable);
            }
            DropKey::Array(s, elems) => {
                let n = 2;
                locals.push(ValType::I32);
                code.extend([I::LocalGet(p), load(Storage::I32, 4), I::LocalSet(n)]);
                if let Some(e) = elems {
                    code.extend([
                        I::LocalGet(p),
                        I::I32Const(8),
                        I::I32Add,
                        I::LocalGet(n),
                        I::Call(self.drop_range(e)),
                    ]);
                }
                code.extend([
                    I::LocalGet(p),
                    I::LocalGet(n),
                    I::I32Const(s.size().trailing_zeros() as i32),
                    I::I32Shl,
                    I::I32Const(8),
                    I::I32Add,
                    I::ReturnCall(self.release()),
                ]);
            }
            DropKey::Ref(_, inner) => {
                let v = 2;
                locals.push(ValType::I32);
                if inner.is_some() {
                    code.extend([I::LocalGet(p), load(Storage::I32, 8), I::LocalSet(v)]);
                }
                code.extend([I::LocalGet(p), I::I32Const(16)]);
                match inner {
                    Some(k) => code.extend([
                        I::Call(self.release()),
                        I::LocalGet(v),
                        I::ReturnCall(self.drop(k)),
                    ]),
                    None => code.push(I::ReturnCall(self.release())),
                }
            }
            DropKey::Closure => code.extend([
                I::LocalGet(p),
                I::LocalGet(p),
                load(Storage::I32, 4),
                I::I32Const(closures),
                I::I32Add,
                I::ReturnCallIndirect {
                    type_index: one,
                    table_index: 0,
                },
            ]),
            DropKey::Any => {
                let (kind, v) = (2, 3);
                locals.extend([ValType::I32, ValType::I32]);
                code.extend([
                    I::LocalGet(p),
                    load(Storage::I32, 4),
                    I::LocalSet(kind),
                    I::LocalGet(p),
                    load(Storage::I32, 8),
                    I::LocalSet(v),
                    I::LocalGet(p),
                    I::I32Const(16),
                    I::Call(self.release()),
                    // A scalar's box holds nothing to drop.
                    I::LocalGet(kind),
                    I::I32Eqz,
                    I::If(BlockType::Empty),
                    I::Return,
                    I::End,
                    I::LocalGet(v),
                    I::LocalGet(kind),
                    I::I32Const(kinds - 1),
                    I::I32Add,
                    I::ReturnCallIndirect {
                        type_index: one,
                        table_index: 0,
                    },
                ]);
            }
        }
        code.push(I::End);
        Body { locals, code }
    }

    /// `drop_reuse(p) -> token` of kind `k`, which is only for data.
    fn drop_reuse_body(&self, cx: &mut Cx<'_>, k: &DropKey) -> Body {
        let DropKey::Data(d) = k else {
            return Body {
                locals: Vec::new(),
                code: vec![I::Unreachable, I::End],
            };
        };
        let (p, h, ctor) = (0, 1, 2);
        let locals = vec![ValType::I32, ValType::I32];
        let mut code = vec![
            I::LocalGet(p),
            load(Storage::I32, 0),
            I::LocalTee(h),
            I::I32Const(1),
            I::I32Ne,
            I::If(BlockType::Empty),
            // Shared or immortal: no memory to reuse.
            I::LocalGet(h),
            I::I32Const(0),
            I::I32GeS,
            I::If(BlockType::Empty),
            I::LocalGet(p),
            I::LocalGet(h),
            I::I32Const(1),
            I::I32Sub,
            store(Storage::I32, 0),
            I::End,
            I::I32Const(0),
            I::Return,
            I::End,
            I::LocalGet(p),
            load(Storage::I32, 4),
            I::LocalSet(ctor),
        ];
        let ctors = cx.m.data.get(d).map_or(&[][..], |d| &d.ctors[..]);
        for (i, c) in ctors.iter().enumerate() {
            let (offsets, _) = layout(&c.fields);
            code.extend([
                I::LocalGet(ctor),
                I::I32Const(i as i32),
                I::I32Eq,
                I::If(BlockType::Empty),
            ]);
            for (t, &(at, _)) in c.fields.iter().zip(&offsets) {
                if let Some(k) = key_of(t) {
                    code.extend([
                        I::LocalGet(p),
                        load(Storage::I32, at),
                        I::Call(self.drop(&k)),
                    ]);
                }
            }
            code.extend([I::LocalGet(p), I::Return, I::End]);
        }
        code.extend([I::Unreachable, I::End]);
        Body { locals, code }
    }

    /// `free(p)` of kind `k`: only its memory.
    fn free_body(&self, cx: &mut Cx<'_>, k: &DropKey) -> Body {
        let p = 0;
        let mut code = Vec::new();
        match k {
            DropKey::Data(d) => {
                let ctors = cx.m.data.get(d).map_or(&[][..], |d| &d.ctors[..]);
                for (i, c) in ctors.iter().enumerate() {
                    let (_, size) = layout(&c.fields);
                    code.extend([
                        I::LocalGet(p),
                        load(Storage::I32, 4),
                        I::I32Const(i as i32),
                        I::I32Eq,
                        I::If(BlockType::Empty),
                        I::LocalGet(p),
                        I::I32Const(size as i32),
                        I::ReturnCall(self.release()),
                        I::End,
                    ]);
                }
                code.push(I::Unreachable);
            }
            DropKey::Array(s, _) => code.extend([
                I::LocalGet(p),
                I::LocalGet(p),
                load(Storage::I32, 4),
                I::I32Const(s.size().trailing_zeros() as i32),
                I::I32Shl,
                I::I32Const(8),
                I::I32Add,
                I::ReturnCall(self.release()),
            ]),
            DropKey::Ref(..) | DropKey::Any => code.extend([
                I::LocalGet(p),
                I::I32Const(16),
                I::ReturnCall(self.release()),
            ]),
            // A closure's size is its wrapper's to know, and nothing frees
            // one without dropping it.
            DropKey::Closure => code.push(I::Unreachable),
        }
        code.push(I::End);
        Body {
            locals: Vec::new(),
            code,
        }
    }

    /// The function that frees a closure of `f` with `n` captures, and
    /// drops them.
    pub fn closure_free(&self, cx: &Cx<'_>, f: conduit_ir::FuncId, n: usize) -> Body {
        let tys = &cx.m.funcs[&f].sig().params[..n];
        let (offsets, size) = layout(tys);
        let held: Vec<(u32, DropKey)> = tys
            .iter()
            .zip(&offsets)
            .filter_map(|(t, &(at, _))| key_of(t).map(|k| (at, k)))
            .collect();
        let mut code = Vec::new();
        let mut locals = Vec::new();
        for (j, (at, _)) in held.iter().enumerate() {
            locals.push(ValType::I32);
            code.extend([
                I::LocalGet(0),
                load(Storage::I32, *at),
                I::LocalSet(1 + j as u32),
            ]);
        }
        code.extend([
            I::LocalGet(0),
            I::I32Const(size as i32),
            I::Call(self.release()),
        ]);
        for (j, (_, k)) in held.iter().enumerate() {
            code.push(I::LocalGet(1 + j as u32));
            code.push(if j + 1 == held.len() {
                I::ReturnCall(self.drop(k))
            } else {
                I::Call(self.drop(k))
            });
        }
        code.push(I::End);
        Body { locals, code }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Dup,
    Add,
    DecRef,
}

fn sig(params: &[ValType], results: &[ValType]) -> crate::FnSig {
    crate::FnSig {
        params: params.to_vec(),
        results: results.to_vec(),
    }
}

/// Returns if the object in local `p` is 0 or immortal; leaves its header
/// in local `h`.
fn skip_uncounted(p: u32, h: u32) -> Vec<I<'static>> {
    vec![
        I::LocalGet(p),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::Return,
        I::End,
        I::LocalGet(p),
        load(Storage::I32, 0),
        I::LocalTee(h),
        I::I32Const(0),
        I::I32LtS,
        I::If(BlockType::Empty),
        I::Return,
        I::End,
    ]
}

/// `is_unique(p)`: whether `p` isn't 0 and its count is 1.
fn is_unique() -> Body {
    Body {
        locals: Vec::new(),
        code: vec![
            I::LocalGet(0),
            I::If(BlockType::Result(ValType::I32)),
            I::LocalGet(0),
            load(Storage::I32, 0),
            I::I32Const(1),
            I::I32Eq,
            I::Else,
            I::I32Const(0),
            I::End,
            I::End,
        ],
    }
}

/// `(address, n)`: calls `each` on the `n` references stored from
/// `address`.
fn range(each: u32) -> Body {
    let (at, n) = (0, 1);
    Body {
        locals: Vec::new(),
        code: vec![
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(n),
            I::I32Eqz,
            I::BrIf(1),
            I::LocalGet(at),
            load(Storage::I32, 0),
            I::Call(each),
            I::LocalGet(at),
            I::I32Const(4),
            I::I32Add,
            I::LocalSet(at),
            I::LocalGet(n),
            I::I32Const(1),
            I::I32Sub,
            I::LocalSet(n),
            I::Br(0),
            I::End,
            I::End,
            I::End,
        ],
    }
}

/// `alloc(size) -> address` with free lists: a block of the size from its
/// list, zeroed, or else from the heap, as the plain `alloc` does. Either
/// way its count is 1.
pub(crate) fn lower_alloc(heads: u32) -> Body {
    let (size, p, prev) = (0, 1, 2);
    let heads = heads as i32;
    let mut code = vec![
        I::LocalGet(size),
        I::I32Const(7),
        I::I32Add,
        I::I32Const(-8),
        I::I32And,
        I::LocalSet(size),
        I::Block(BlockType::Empty), // done
        I::Block(BlockType::Empty), // reuse
        I::LocalGet(size),
        I::I32Const(3),
        I::I32ShrU,
        I::I32Const(CLASSES as i32),
        I::I32LeU,
        I::If(BlockType::Empty),
        // Its size class's list.
        I::LocalGet(size),
        I::I32Const(3),
        I::I32ShrU,
        I::I32Const(2),
        I::I32Shl,
        I::I32Const(heads),
        I::I32Add,
        I::LocalTee(prev),
        load(Storage::I32, 0),
        I::LocalTee(p),
        I::If(BlockType::Empty),
        I::LocalGet(prev),
        I::LocalGet(p),
        load(Storage::I32, 0),
        store(Storage::I32, 0),
        I::Br(2), // reuse
        I::End,
        I::Else,
        // The big blocks', for one of the same size.
        I::I32Const(heads),
        I::LocalSet(prev),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(prev),
        load(Storage::I32, 0),
        I::LocalTee(p),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(p),
        load(Storage::I32, 4),
        I::LocalGet(size),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::LocalGet(prev),
        I::LocalGet(p),
        load(Storage::I32, 0),
        store(Storage::I32, 0),
        I::Br(4), // reuse
        I::End,
        I::LocalGet(p),
        I::LocalSet(prev),
        I::Br(0),
        I::End,
        I::End,
        I::End,
    ];
    // Nothing to reuse: bump, as the plain `alloc` does, which leaves the
    // address.
    let bump = crate::func::lower_alloc();
    let bump = &bump.code[6..bump.code.len() - 1];
    code.extend(bump.iter().cloned());
    code.extend([
        I::LocalSet(p),
        I::Br(1), // done
        I::End,
        // Reused memory isn't zero.
        I::LocalGet(p),
        I::I32Const(0),
        I::LocalGet(size),
        I::MemoryFill(0),
        I::End,
        I::LocalGet(p),
        I::I32Const(1),
        store(Storage::I32, 0),
        I::GlobalGet(LIVE),
        I::I32Const(1),
        I::I32Add,
        I::GlobalSet(LIVE),
        I::LocalGet(p),
        I::End,
    ]);
    Body {
        locals: vec![ValType::I32, ValType::I32, ValType::I32],
        code,
    }
}

fn children<'b>(b: &'b Block, out: &mut Vec<&'b Block>) {
    match &b.tail {
        Tail::Join { defs, body, .. } => {
            out.extend(defs.iter().map(|d| &d.body));
            out.push(body);
        }
        Tail::Case { arms, default, .. } => {
            out.extend(arms.iter().map(|a| &a.body));
            out.extend(default.as_deref());
        }
        _ => {}
    }
}
