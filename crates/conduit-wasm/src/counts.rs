//! Reference counting in linear memory, for a module whose counts are
//! explicit (see "Reference counts" in `docs/ir.md`), when
//! [`Options::counts`](crate::Options::counts) asks for it, and cycle
//! collection, when [`Options::cycles`](crate::Options::cycles) does too.
//!
//! - **The header word** is the count, in bits 0 to 27, then the color of
//!   trial deletion in bits 28 and 29, whether the object is buffered as a
//!   possible root of a cycle in bit 30, and in bit 31, whether it's
//!   immortal, as constants are, which counting skips. A count that
//!   would pass its 28 bits saturates: the object turns immortal and
//!   leaks, rather than being freed while it's still in use.
//! - **Memory** comes back through `release`, onto a free list per size,
//!   one for each multiple of 8 up to 256 bytes and one for bigger blocks,
//!   whose heads are a table in memory after the constants. `alloc` takes
//!   a block from its list, of the same size for a big one, before it
//!   bumps the heap, zeroes it, and sets its count to 1.
//! - **Dropping** goes through functions for each kind of object, a
//!   [`DropKey`]: a data type, arrays and refs of what they hold,
//!   closures, continuations and `any`. At 0, `drop` frees the object and
//!   drops what it held, tail-calling the drop of its last field, so that
//!   dropping a list is a loop. A closure's or continuation's own function
//!   frees it, found in the table from its wrapper's slot, and an `any` is
//!   always a box, which holds the number of the kind of what it holds.
//! - **Cycles** are collected by trial deletion (Bacon and Rajan,
//!   synchronous). A `drop` that leaves an object above 0 buffers it, if
//!   its kind can be in a cycle: if the kinds' graph, without the fields
//!   that are `imm`, leads back to it. Each kind has two drops: one for
//!   objects whose type is `imm`, which never buffers, since an `imm`
//!   object can't be in a cycle, and only drops `imm` objects, and one for
//!   the rest. The buffer holds 256 objects with their kinds, and is
//!   collected when it's full: `mark_gray`, `scan`, `scan_black` and
//!   `collect_white` follow what an object holds through its kind's
//!   `children`, which does one step of them on each child that can be in
//!   a cycle, and drops the others of a white object. A step pushes the
//!   child on a stack in memory, which each of them pops until it's back
//!   where it started, so the collector doesn't recurse. A buffered
//!   object's memory is only ever given back by the collector, so the
//!   buffer never points to memory that holds another object.
//!
//! The functions come right after `alloc`, in the order of the methods
//! below, so their indices are known before any code is lowered: the
//! kinds are found up front, from every type in the module.

use std::collections::{BTreeMap, BTreeSet};

use conduit_ir::{DataId, FuncId, Module, Mutability, Op, Ty};
use wasm_encoder::{BlockType, Instruction as I, ValType};

use crate::func::{Body, load, store};
use crate::{Cx, Storage, layout};

/// The global counting live objects, which tests compare with the
/// evaluator's.
pub(crate) const LIVE: u32 = 3;

/// The global holding how many objects are buffered.
pub(crate) const ROOTS: u32 = 4;

/// The global that's 1 while cycles are collected, so that what's buffered
/// meanwhile doesn't start another collection.
pub(crate) const COLLECTING: u32 = 5;

/// The globals of the collector's stack: its address, how many entries it
/// has room for, and how many it holds.
const STACK: u32 = 6;
const CAP: u32 = 7;
const TOP: u32 = 8;

/// Size classes have free lists up to this many 8-byte words.
const CLASSES: u32 = 32;

/// The buffer is collected once it holds this many objects. During a
/// collection, it can grow to twice that; past that, possible roots are
/// lost, and their cycles leak.
const BUFFER: u32 = 256;

/// The header's bits.
const COUNT: i32 = 0x0fff_ffff;
const COLOR: i32 = 0x3000_0000;
const BUFFERED: i32 = 0x4000_0000;
/// The sign bit: the object is never counted, as constants and objects
/// whose count saturated aren't.
const IMMORTAL: i32 = i32::MIN;
/// The count and whether it's immortal, without the collector's bits.
const COUNTED: i32 = 0x8fff_ffffu32 as i32;

/// The colors of trial deletion, in the header's bits 28 and 29: black is
/// in use, or not looked at, gray is being looked at, white is garbage,
/// and purple is a possible root of a garbage cycle.
const BLACK: i32 = 0;
const GRAY: i32 = 0x1000_0000;
const WHITE: i32 = 0x2000_0000;
const PURPLE: i32 = 0x3000_0000;

/// What `children` does with each child, and the step it does to it: one
/// of trial deletion's, or, for `FREE`, giving back only the object's
/// memory.
const OP_GRAY: i32 = 0;
const OP_SCAN: i32 = 1;
const OP_BLACK: i32 = 2;
const OP_WHITE: i32 = 3;
const OP_FREE: i32 = 4;

/// The functions every counted module has, before those of each kind.
const FIXED: u32 = 13;

/// The functions of each kind.
const PER_KIND: u32 = 8;

/// A kind of object that's dropped the same way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DropKey {
    Data(DataId),
    /// What its elements are stored as, how they're dropped, and whether
    /// their type is `imm`, so they can't be in a cycle.
    Array(Storage, Option<Box<DropKey>>, bool),
    Ref(Storage, Option<Box<DropKey>>, bool),
    Closure,
    /// A Conduit continuation, of a handler that switches stacks: a
    /// closure-like object whose own functions free it and step through
    /// it, found in the table from its wrapper's slot (see `switch.rs`).
    Cont,
    Any,
    /// A box of a host value: its slot in the table of `externref`s (see
    /// [`Hosts`](crate::Hosts)). It holds nothing that's counted, so it's
    /// never in a cycle.
    Host,
}

/// How a value of type `ty` is dropped, or `None` if it isn't counted.
pub(crate) fn key_of(ty: &Ty) -> Option<DropKey> {
    Some(match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::F32 | Ty::F64 | Ty::Reuse(_) => {
            return None;
        }
        Ty::Cont(_) => DropKey::Cont,
        Ty::Data(d, ..) => DropKey::Data(*d),
        Ty::Array(e, _) => DropKey::Array(Storage::of(e), key_of(e).map(Box::new), is_imm(e)),
        Ty::Ref(t) => DropKey::Ref(Storage::of(t), key_of(t).map(Box::new), is_imm(t)),
        Ty::Fn(..) => DropKey::Closure,
        Ty::Any(_) => DropKey::Any,
        Ty::Host | Ty::NullableHost => DropKey::Host,
    })
}

/// Whether a value of type `ty` is `imm`, so its object can't be in a
/// cycle.
pub(crate) fn is_imm(ty: &Ty) -> bool {
    ty.mutability() == Some(Mutability::Imm)
}

/// Where the tables hold the functions [`Runtime::bodies`] calls through
/// them: a closure's at its wrapper's slot plus an offset, and a kind's at
/// its number plus a base.
#[derive(Clone, Copy)]
pub(crate) struct Slots {
    pub closure_free: i32,
    /// Added to a continuation's wrapper's slot: the slots of its free
    /// function and of its children.
    pub kont_free: i32,
    pub kont_children: i32,
    pub closure_children: i32,
    pub kind_drop: i32,
    pub kind_children: i32,
    pub kind_step: i32,
}

/// The functions of reference counting, and where things are.
pub(crate) struct Runtime {
    /// The index of the first function.
    base: u32,
    pub keys: Vec<DropKey>,
    index: BTreeMap<DropKey, u32>,
    /// Whether each kind can be in a cycle.
    cyclic: Vec<bool>,
    /// Whether cycles are collected.
    pub cycles: bool,
    /// The address of the free lists' heads: the big blocks', then one per
    /// size class from 8 bytes. The buffer of possible roots comes right
    /// after.
    pub heads: u32,
}

impl Runtime {
    /// Finds the kinds of `m`'s objects, for functions from `base`.
    pub fn new(m: &Module, base: u32, cycles: bool) -> Runtime {
        let mut rt = Runtime {
            base,
            keys: Vec::new(),
            index: BTreeMap::new(),
            cyclic: Vec::new(),
            cycles,
            heads: 0,
        };
        let mut tys: Vec<&Ty> = Vec::new();
        for f in m.funcs.values() {
            tys.extend(&f.vars);
            tys.extend(&f.results);
            for b in f.body.blocks() {
                for l in &b.lets {
                    if let Op::Box(t) | Op::Unbox(t) = &l.op {
                        tys.push(t);
                    }
                }
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
                DropKey::Array(_, Some(e), _) | DropKey::Ref(_, Some(e), _) => {
                    todo.push((**e).clone())
                }
                _ => {}
            }
            rt.keys.push(k);
        }
        // A kind can be in a cycle if what its objects hold, but through
        // `imm` fields, leads back to it. A closure or an `any` can hold
        // any kind.
        let n = rt.keys.len();
        let edges: Vec<Vec<usize>> = (rt.keys.iter())
            .map(|k| match k {
                DropKey::Data(d) => {
                    let fields = m.data.get(d).into_iter().flat_map(|d| &d.ctors);
                    (fields.flat_map(|c| &c.fields))
                        .filter(|t| !is_imm(t))
                        .filter_map(key_of)
                        .map(|k| rt.index[&k] as usize)
                        .collect()
                }
                DropKey::Array(_, Some(e), false) | DropKey::Ref(_, Some(e), false) => {
                    vec![rt.index[e.as_ref()] as usize]
                }
                DropKey::Array(..) | DropKey::Ref(..) | DropKey::Host => Vec::new(),
                DropKey::Closure | DropKey::Cont | DropKey::Any => (0..n).collect(),
            })
            .collect();
        rt.cyclic = (0..n)
            .map(|k| {
                let mut seen = BTreeSet::new();
                let mut todo = edges[k].clone();
                while let Some(j) = todo.pop() {
                    if j == k {
                        return true;
                    }
                    if seen.insert(j) {
                        todo.extend(&edges[j]);
                    }
                }
                false
            })
            .collect();
        rt
    }

    /// How many functions it takes.
    pub fn len(&self) -> u32 {
        FIXED + PER_KIND * self.keys.len() as u32
    }

    /// The address of the buffer of possible roots, and how many bytes the
    /// heads and the buffer take.
    pub fn memory(&self) -> u32 {
        (4 * 33u32).next_multiple_of(8) + 8 * 2 * BUFFER
    }

    fn roots(&self) -> i32 {
        (self.heads + (4 * 33u32).next_multiple_of(8)) as i32
    }

    /// `dup(p)`.
    pub fn dup(&self) -> u32 {
        self.base
    }

    /// `add(p, n)`: adds `n` to the count.
    pub fn add(&self) -> u32 {
        self.base + 1
    }

    /// `is_unique(p) -> i32`.
    pub fn is_unique(&self) -> u32 {
        self.base + 2
    }

    /// `release(p, size)`: gives the memory back.
    pub fn release(&self) -> u32 {
        self.base + 3
    }

    /// `dup_range(address, n)`: dups `n` references stored from `address`.
    pub fn dup_range(&self) -> u32 {
        self.base + 4
    }

    /// `release_obj(p, size)`: gives an object's memory back, unless it's
    /// buffered, which the collector then does.
    fn release_obj(&self) -> u32 {
        self.base + 5
    }

    /// `possible_root(p, kind)`.
    fn possible_root(&self) -> u32 {
        self.base + 6
    }

    /// `collect()`: collects the buffer's cycles.
    pub fn collect(&self) -> u32 {
        self.base + 7
    }

    /// `mark_gray(p, kind)`, `scan(p, kind)`, `scan_black(p, kind)` and
    /// `collect_white(p, kind)`.
    fn mark_gray(&self) -> u32 {
        self.base + 8
    }

    fn scan(&self) -> u32 {
        self.base + 9
    }

    fn scan_black(&self) -> u32 {
        self.base + 10
    }

    fn collect_white(&self) -> u32 {
        self.base + 11
    }

    /// `push(p, kind)` onto the collector's stack.
    fn push(&self) -> u32 {
        self.base + 12
    }

    /// The number of a kind. A box holds it plus 1.
    pub fn number(&self, k: &DropKey) -> u32 {
        self.index[k]
    }

    fn kind(&self, k: &DropKey) -> u32 {
        self.base + FIXED + PER_KIND * self.number(k)
    }

    /// `drop(p)` of a kind, of an object whose type is `imm` if `imm`.
    pub fn drop(&self, k: &DropKey, imm: bool) -> u32 {
        self.kind(k) + imm as u32
    }

    /// The `drop` for a value of type `ty`, if it's counted.
    pub fn drop_of(&self, ty: &Ty) -> Option<u32> {
        key_of(ty).map(|k| self.drop(&k, is_imm(ty)))
    }

    /// `drop_range(address, n)`: drops `n` references of a kind stored
    /// from `address`.
    pub fn drop_range(&self, k: &DropKey, imm: bool) -> u32 {
        self.kind(k) + 2 + imm as u32
    }

    /// `free(p)` of a kind: gives its memory back without dropping what it
    /// holds.
    pub fn free(&self, k: &DropKey) -> u32 {
        self.kind(k) + 4
    }

    /// `drop_reuse(p) -> token` of a data type: like `drop`, but at 0 it
    /// keeps the memory, for the token, which is otherwise 0.
    pub fn drop_reuse(&self, k: &DropKey) -> u32 {
        self.kind(k) + 5
    }

    /// `step(c, op)`: one step of trial deletion on a child of this kind.
    pub fn step(&self, k: &DropKey) -> u32 {
        self.kind(k) + 6
    }

    /// `children(p, op)`: `step` on each child of an object of this kind.
    pub fn children(&self, k: &DropKey) -> u32 {
        self.kind(k) + 7
    }

    /// The bodies of the functions, with their types.
    pub fn bodies(&self, cx: &mut Cx<'_>, slots: Slots) -> Vec<(u32, Body)> {
        let one = cx.sig(sig(&[ValType::I32], &[]));
        let two = cx.sig(sig(&[ValType::I32, ValType::I32], &[]));
        let test = cx.sig(sig(&[ValType::I32], &[ValType::I32]));
        let none = cx.sig(sig(&[], &[]));
        let mut out = vec![
            (one, self.change(false)),
            (two, self.change(true)),
            (test, is_unique()),
            (two, self.release_body()),
            (two, range(self.dup())),
            (two, self.release_obj_body()),
            (two, self.possible_root_body()),
            (none, self.collect_body(two, slots)),
            (two, self.mark_gray_body(two, slots)),
            (two, self.scan_body(two, slots)),
            (two, self.scan_black_body(two, slots)),
            (two, self.collect_white_body(two, slots)),
            (two, self.push_body()),
        ];
        for k in &self.keys {
            out.push((one, self.drop_body(cx, k, false, one, slots)));
            out.push((one, self.drop_body(cx, k, true, one, slots)));
            out.push((two, range(self.drop(k, false))));
            out.push((two, range(self.drop(k, true))));
            out.push((one, self.free_body(cx, k)));
            out.push((test, self.drop_reuse_body(cx, k)));
            out.push((two, self.step_body(k)));
            out.push((two, self.children_body(cx, k, two, slots)));
        }
        out
    }

    /// `dup` and `add`: add 1 or `n` to the count, unless the object is 0
    /// (undefined) or immortal. It's black then, as trial deletion wants.
    fn change(&self, add: bool) -> Body {
        let (p, h) = (0, if add { 2 } else { 1 });
        let n = if add { I::LocalGet(1) } else { I::I32Const(1) };
        let mut code = skip_uncounted(p, h);
        // A count that would pass its bits saturates: the object turns
        // immortal, which every count skips, so it's never freed. It
        // leaks, but nothing it holds is freed early. Its count bits stay
        // set, so that the collector takes it out of the buffer without
        // freeing it, and its color is black, as for any `dup`.
        code.extend([
            I::LocalGet(h),
            I::I32Const(COUNT),
            I::I32And,
            n.clone(),
            I::I32Add,
            I::I32Const(COUNT),
            I::I32GtU,
            I::If(BlockType::Empty),
            I::LocalGet(p),
            I::LocalGet(h),
            I::I32Const(!COLOR),
            I::I32And,
            I::I32Const(COUNT | IMMORTAL),
            I::I32Or,
            store(Storage::I32, 0),
            I::Return,
            I::End,
            I::LocalGet(p),
            I::LocalGet(h),
            n,
            I::I32Add,
            I::I32Const(!COLOR),
            I::I32And,
            store(Storage::I32, 0),
            I::End,
        ]);
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

    /// `release_obj(p, size)`: a buffered object keeps its memory, with a
    /// count of 0, for the collector to give back.
    fn release_obj_body(&self) -> Body {
        let code = vec![
            I::LocalGet(0),
            load(Storage::I32, 0),
            I::I32Const(BUFFERED),
            I::I32And,
            I::If(BlockType::Empty),
            I::LocalGet(0),
            I::I32Const(BUFFERED),
            store(Storage::I32, 0),
            I::Return,
            I::End,
            I::LocalGet(0),
            I::LocalGet(1),
            I::ReturnCall(self.release()),
            I::End,
        ];
        Body {
            locals: Vec::new(),
            code,
        }
    }

    /// `possible_root(p, kind)`: makes it purple, and buffers it if it
    /// isn't, collecting once the buffer is full.
    fn possible_root_body(&self) -> Body {
        let (p, kind, h, at) = (0, 1, 2, 3);
        let roots = self.roots();
        let code = vec![
            I::LocalGet(p),
            load(Storage::I32, 0),
            I::LocalTee(h),
            I::I32Const(COLOR),
            I::I32And,
            I::I32Const(PURPLE),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::Return,
            I::End,
            I::LocalGet(h),
            I::I32Const(PURPLE),
            I::I32Or,
            I::LocalSet(h),
            // Buffered already, or no room left.
            I::LocalGet(h),
            I::I32Const(BUFFERED),
            I::I32And,
            I::GlobalGet(ROOTS),
            I::I32Const(2 * BUFFER as i32),
            I::I32GeU,
            I::I32Or,
            I::If(BlockType::Empty),
            I::LocalGet(p),
            I::LocalGet(h),
            store(Storage::I32, 0),
            I::Return,
            I::End,
            I::LocalGet(p),
            I::LocalGet(h),
            I::I32Const(BUFFERED),
            I::I32Or,
            store(Storage::I32, 0),
            I::GlobalGet(ROOTS),
            I::I32Const(3),
            I::I32Shl,
            I::I32Const(roots),
            I::I32Add,
            I::LocalTee(at),
            I::LocalGet(p),
            store(Storage::I32, 0),
            I::LocalGet(at),
            I::LocalGet(kind),
            store(Storage::I32, 4),
            I::GlobalGet(ROOTS),
            I::I32Const(1),
            I::I32Add,
            I::GlobalSet(ROOTS),
            I::GlobalGet(ROOTS),
            I::I32Const(BUFFER as i32),
            I::I32GeU,
            I::GlobalGet(COLLECTING),
            I::I32Eqz,
            I::I32And,
            I::If(BlockType::Empty),
            I::Call(self.collect()),
            I::End,
            I::End,
        ];
        Body {
            locals: vec![ValType::I32, ValType::I32],
            code,
        }
    }

    /// `collect()`: trial deletion of the objects buffered so far. Those
    /// buffered while it runs, by dropping what garbage held, stay in the
    /// buffer, moved to its start.
    fn collect_body(&self, two: u32, slots: Slots) -> Body {
        let (n, i, at, p, kind, h) = (0, 1, 2, 3, 4, 5);
        let roots = self.roots();
        // Each root in turn: `at` its entry, `p` and `kind` from it.
        let each = |body: Vec<I<'static>>| -> Vec<I<'static>> {
            let mut code = vec![
                I::I32Const(0),
                I::LocalSet(i),
                I::Block(BlockType::Empty),
                I::Loop(BlockType::Empty),
                I::LocalGet(i),
                I::LocalGet(n),
                I::I32GeU,
                I::BrIf(1),
                I::LocalGet(i),
                I::I32Const(3),
                I::I32Shl,
                I::I32Const(roots),
                I::I32Add,
                I::LocalTee(at),
                load(Storage::I32, 0),
                I::LocalSet(p),
                I::LocalGet(at),
                load(Storage::I32, 4),
                I::LocalSet(kind),
                I::Block(BlockType::Empty),
                I::LocalGet(p),
                I::I32Eqz,
                I::BrIf(0),
            ];
            code.extend(body);
            code.extend([
                I::End,
                I::LocalGet(i),
                I::I32Const(1),
                I::I32Add,
                I::LocalSet(i),
                I::Br(0),
                I::End,
                I::End,
            ]);
            code
        };
        let mut code = vec![
            I::I32Const(1),
            I::GlobalSet(COLLECTING),
            I::GlobalGet(ROOTS),
            I::LocalSet(n),
        ];
        // Mark gray from each root that's still purple. The others leave the
        // buffer, and if they're black with a count of 0, dropped while
        // buffered, their memory goes back. A gray one's count can be 0 only
        // for now: an earlier root's traversal took away what it holds.
        code.extend(each(vec![
            I::LocalGet(p),
            load(Storage::I32, 0),
            I::LocalTee(h),
            I::I32Const(COLOR),
            I::I32And,
            I::I32Const(PURPLE),
            I::I32Eq,
            I::LocalGet(h),
            I::I32Const(COUNT),
            I::I32And,
            I::I32Const(0),
            I::I32Ne,
            I::I32And,
            I::If(BlockType::Empty),
            I::LocalGet(p),
            I::LocalGet(kind),
            I::Call(self.mark_gray()),
            I::Else,
            I::LocalGet(p),
            I::LocalGet(h),
            I::I32Const(!BUFFERED),
            I::I32And,
            store(Storage::I32, 0),
            I::LocalGet(h),
            I::I32Const(COUNT | COLOR),
            I::I32And,
            I::I32Eqz,
            I::If(BlockType::Empty),
            I::LocalGet(p),
            I::I32Const(OP_FREE),
            I::LocalGet(kind),
            I::I32Const(slots.kind_children),
            I::I32Add,
            I::CallIndirect {
                type_index: two,
                table_index: 0,
            },
            I::End,
            I::LocalGet(at),
            I::I32Const(0),
            store(Storage::I32, 0),
            I::End,
        ]));
        code.extend(each(vec![
            I::LocalGet(p),
            I::LocalGet(kind),
            I::Call(self.scan()),
        ]));
        code.extend(each(vec![
            I::LocalGet(p),
            I::LocalGet(p),
            load(Storage::I32, 0),
            I::I32Const(!BUFFERED),
            I::I32And,
            store(Storage::I32, 0),
            I::LocalGet(p),
            I::LocalGet(kind),
            I::Call(self.collect_white()),
        ]));
        // What was buffered meanwhile goes to the start.
        code.extend([
            I::I32Const(roots),
            I::LocalGet(n),
            I::I32Const(3),
            I::I32Shl,
            I::I32Const(roots),
            I::I32Add,
            I::GlobalGet(ROOTS),
            I::LocalGet(n),
            I::I32Sub,
            I::LocalTee(n),
            I::I32Const(3),
            I::I32Shl,
            I::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            },
            I::LocalGet(n),
            I::GlobalSet(ROOTS),
            I::I32Const(0),
            I::GlobalSet(COLLECTING),
            I::End,
        ]);
        Body {
            locals: vec![ValType::I32; 6],
            code,
        }
    }

    /// A phase of trial deletion: `first` on the object in local 0, then
    /// `body` on it, `u`, of kind `uk`, and on each object the steps push,
    /// popped from the stack until it's back where it was. `body` has the
    /// header in local `h`.
    fn phase(&self, first: Vec<I<'static>>, body: Vec<I<'static>>) -> Body {
        let (p, kind, base, at, u, uk) = (0, 1, 3, 4, 5, 6);
        let mut code = first;
        code.extend([
            I::GlobalGet(TOP),
            I::LocalSet(base),
            I::LocalGet(p),
            I::LocalGet(kind),
            I::Call(self.push()),
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::GlobalGet(TOP),
            I::LocalGet(base),
            I::I32LeU,
            I::BrIf(1),
            I::GlobalGet(TOP),
            I::I32Const(1),
            I::I32Sub,
            I::GlobalSet(TOP),
            I::GlobalGet(STACK),
            I::GlobalGet(TOP),
            I::I32Const(3),
            I::I32Shl,
            I::I32Add,
            I::LocalTee(at),
            load(Storage::I32, 0),
            I::LocalSet(u),
            I::LocalGet(at),
            load(Storage::I32, 4),
            I::LocalSet(uk),
        ]);
        code.extend(body);
        code.extend([I::Br(0), I::End, I::End, I::End]);
        Body {
            locals: vec![ValType::I32; 5],
            code,
        }
    }

    /// `mark_gray(p, kind)`: gray, then the step on each child, which
    /// takes 1 from its count and marks it too.
    fn mark_gray_body(&self, two: u32, slots: Slots) -> Body {
        let (u, uk, h) = (5, 6, 2);
        let mut body = color_is(u, h, GRAY);
        body.extend([I::I32Eqz, I::If(BlockType::Empty)]);
        body.extend(set_color(u, h, GRAY));
        body.extend(children_call(u, uk, OP_GRAY, two, slots));
        body.push(I::End);
        self.phase(Vec::new(), body)
    }

    /// `scan(p, kind)`: a gray object is black again if it's still above 0,
    /// with what it reaches, or else white, then each child is scanned.
    fn scan_body(&self, two: u32, slots: Slots) -> Body {
        let (u, uk, h) = (5, 6, 2);
        let mut body = color_is(u, h, GRAY);
        body.extend([
            I::If(BlockType::Empty),
            I::LocalGet(h),
            I::I32Const(COUNT),
            I::I32And,
            I::If(BlockType::Empty),
            I::LocalGet(u),
            I::LocalGet(uk),
            I::Call(self.scan_black()),
            I::Else,
        ]);
        body.extend(set_color(u, h, WHITE));
        body.extend(children_call(u, uk, OP_SCAN, two, slots));
        body.extend([I::End, I::End]);
        self.phase(Vec::new(), body)
    }

    /// `scan_black(p, kind)`: black, then the step on each child, which
    /// gives back the count `mark_gray` took, and makes it black too if it
    /// isn't. What's on the stack is black already.
    fn scan_black_body(&self, two: u32, slots: Slots) -> Body {
        let (p, u, uk, h) = (0, 5, 6, 2);
        let mut first = vec![I::LocalGet(p), load(Storage::I32, 0), I::LocalSet(h)];
        first.extend(set_color(p, h, BLACK));
        self.phase(first, children_call(u, uk, OP_BLACK, two, slots))
    }

    /// `collect_white(p, kind)`: a white object that isn't buffered is
    /// garbage: black, so it's done once, then what it holds, then its
    /// memory, which `children` gives back.
    fn collect_white_body(&self, two: u32, slots: Slots) -> Body {
        let (u, uk, h) = (5, 6, 2);
        let mut body = color_is(u, h, WHITE);
        body.extend([
            I::LocalGet(h),
            I::I32Const(BUFFERED),
            I::I32And,
            I::I32Eqz,
            I::I32And,
            I::If(BlockType::Empty),
        ]);
        body.extend(set_color(u, h, BLACK));
        body.extend(children_call(u, uk, OP_WHITE, two, slots));
        body.push(I::End);
        self.phase(Vec::new(), body)
    }

    /// `push(p, kind)`: onto the collector's stack, a block from `alloc`
    /// that doubles when it's full. It isn't an object, so it isn't live.
    fn push_body(&self) -> Body {
        let (p, kind, cap, new) = (0, 1, 2, 3);
        let alloc = self.base - 1;
        let code = vec![
            I::GlobalGet(TOP),
            I::GlobalGet(CAP),
            I::I32GeU,
            I::If(BlockType::Empty),
            I::GlobalGet(CAP),
            I::I32Const(1),
            I::I32Shl,
            I::LocalTee(cap),
            I::I32Eqz,
            I::If(BlockType::Empty),
            I::I32Const(64),
            I::LocalSet(cap),
            I::End,
            I::LocalGet(cap),
            I::I32Const(3),
            I::I32Shl,
            I::Call(alloc),
            I::LocalSet(new),
            I::GlobalGet(LIVE),
            I::I32Const(1),
            I::I32Sub,
            I::GlobalSet(LIVE),
            I::GlobalGet(STACK),
            I::If(BlockType::Empty),
            I::LocalGet(new),
            I::GlobalGet(STACK),
            I::GlobalGet(TOP),
            I::I32Const(3),
            I::I32Shl,
            I::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            },
            I::GlobalGet(LIVE),
            I::I32Const(1),
            I::I32Add,
            I::GlobalSet(LIVE),
            I::GlobalGet(STACK),
            I::GlobalGet(CAP),
            I::I32Const(3),
            I::I32Shl,
            I::Call(self.release()),
            I::End,
            I::LocalGet(new),
            I::GlobalSet(STACK),
            I::LocalGet(cap),
            I::GlobalSet(CAP),
            I::End,
            I::GlobalGet(STACK),
            I::GlobalGet(TOP),
            I::I32Const(3),
            I::I32Shl,
            I::I32Add,
            I::LocalTee(new),
            I::LocalGet(p),
            store(Storage::I32, 0),
            I::LocalGet(new),
            I::LocalGet(kind),
            store(Storage::I32, 4),
            I::GlobalGet(TOP),
            I::I32Const(1),
            I::I32Add,
            I::GlobalSet(TOP),
            I::End,
        ];
        Body {
            locals: vec![ValType::I32, ValType::I32],
            code,
        }
    }

    /// `drop(p)` of kind `k`, for an object whose type is `imm` if `imm`.
    fn drop_body(&self, cx: &mut Cx<'_>, k: &DropKey, imm: bool, one: u32, slots: Slots) -> Body {
        let (p, h) = (0, 1);
        let mut code = skip_uncounted(p, h);
        // Shared: one less, and maybe the root of a garbage cycle.
        code.extend([
            I::LocalGet(h),
            I::I32Const(COUNT),
            I::I32And,
            I::I32Const(1),
            I::I32Ne,
            I::If(BlockType::Empty),
            I::LocalGet(p),
            I::LocalGet(h),
            I::I32Const(1),
            I::I32Sub,
            store(Storage::I32, 0),
        ]);
        if self.cycles && !imm && self.cyclic[self.number(k) as usize] {
            code.extend([
                I::LocalGet(p),
                I::I32Const(self.number(k) as i32),
                I::Call(self.possible_root()),
            ]);
        }
        code.extend([I::Return, I::End]);
        // What a field of type `t` is dropped with.
        let drop_of = |t: &Ty, k: &DropKey| self.drop(k, imm || is_imm(t));
        let mut locals = vec![ValType::I32];
        match k {
            DropKey::Data(d) => {
                let ctor = 2;
                locals.push(ValType::I32);
                code.extend([I::LocalGet(p), load(Storage::I32, 4), I::LocalSet(ctor)]);
                let ctors = cx.m.data.get(d).map_or(&[][..], |d| &d.ctors[..]);
                for (i, c) in ctors.iter().enumerate() {
                    let (offsets, size) = layout(&c.fields);
                    let held: Vec<(u32, u32)> = (c.fields.iter().zip(&offsets))
                        .filter_map(|(t, &(at, _))| key_of(t).map(|k| (at, drop_of(t, &k))))
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
                        I::Call(self.release_obj()),
                    ]);
                    for (j, (_, f)) in held.iter().enumerate() {
                        code.push(I::LocalGet(first + j as u32));
                        code.push(if j + 1 == held.len() {
                            I::ReturnCall(*f)
                        } else {
                            I::Call(*f)
                        });
                    }
                    code.extend([I::Return, I::End]);
                }
                code.push(I::Unreachable);
            }
            DropKey::Array(s, elems, elems_imm) => {
                let n = 2;
                locals.push(ValType::I32);
                code.extend([I::LocalGet(p), load(Storage::I32, 4), I::LocalSet(n)]);
                if let Some(e) = elems {
                    code.extend([
                        I::LocalGet(p),
                        I::I32Const(8),
                        I::I32Add,
                        I::LocalGet(n),
                        I::Call(self.drop_range(e, imm || *elems_imm)),
                    ]);
                }
                code.extend([
                    I::LocalGet(p),
                    I::LocalGet(n),
                    I::I32Const(s.size().trailing_zeros() as i32),
                    I::I32Shl,
                    I::I32Const(8),
                    I::I32Add,
                    I::ReturnCall(self.release_obj()),
                ]);
            }
            DropKey::Ref(_, inner, inner_imm) => {
                let v = 2;
                locals.push(ValType::I32);
                if inner.is_some() {
                    code.extend([I::LocalGet(p), load(Storage::I32, 8), I::LocalSet(v)]);
                }
                code.extend([I::LocalGet(p), I::I32Const(16)]);
                match inner {
                    // A ref is `mut`, so what it holds can be anything.
                    Some(k) => code.extend([
                        I::Call(self.release_obj()),
                        I::LocalGet(v),
                        I::ReturnCall(self.drop(k, imm || *inner_imm)),
                    ]),
                    None => code.push(I::ReturnCall(self.release_obj())),
                }
            }
            DropKey::Closure | DropKey::Cont => code.extend([
                I::LocalGet(p),
                I::LocalGet(p),
                load(Storage::I32, 4),
                I::I32Const(if *k == DropKey::Cont {
                    slots.kont_free
                } else {
                    slots.closure_free
                }),
                I::I32Add,
                I::ReturnCallIndirect {
                    type_index: one,
                    table_index: 0,
                },
            ]),
            DropKey::Host => code.extend(recycle_host(cx, p)),
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
                    I::Call(self.release_obj()),
                    // A scalar's box holds nothing to drop.
                    I::LocalGet(kind),
                    I::I32Eqz,
                    I::If(BlockType::Empty),
                    I::Return,
                    I::End,
                    I::LocalGet(v),
                    I::LocalGet(kind),
                    I::I32Const(slots.kind_drop - 1),
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
                        I::ReturnCall(self.release_obj()),
                        I::End,
                    ]);
                }
                code.push(I::Unreachable);
            }
            DropKey::Array(s, ..) => code.extend([
                I::LocalGet(p),
                I::LocalGet(p),
                load(Storage::I32, 4),
                I::I32Const(s.size().trailing_zeros() as i32),
                I::I32Shl,
                I::I32Const(8),
                I::I32Add,
                I::ReturnCall(self.release_obj()),
            ]),
            DropKey::Ref(..) | DropKey::Any => code.extend([
                I::LocalGet(p),
                I::I32Const(16),
                I::ReturnCall(self.release_obj()),
            ]),
            // A closure's size is its wrapper's to know, and nothing frees
            // one without dropping it, nor a continuation.
            DropKey::Closure | DropKey::Cont => code.push(I::Unreachable),
            DropKey::Host => code.extend(recycle_host(cx, p)),
        }
        code.push(I::End);
        Body {
            locals: Vec::new(),
            code,
        }
    }

    /// `drop_reuse(p) -> token` of kind `k`, which is only for data. A
    /// buffered object's memory is the collector's, so it gives none.
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
            I::I32Const(COUNTED | BUFFERED),
            I::I32And,
            I::I32Const(1),
            I::I32Ne,
            I::If(BlockType::Empty),
            // Shared, immortal or buffered: a plain drop, and no memory.
            I::LocalGet(p),
            I::Call(self.drop(k, false)),
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
                if let Some(f) = self.drop_of(t) {
                    code.extend([I::LocalGet(p), load(Storage::I32, at), I::Call(f)]);
                }
            }
            code.extend([I::LocalGet(p), I::Return, I::End]);
        }
        code.extend([I::Unreachable, I::End]);
        Body { locals, code }
    }

    /// `step(c, op)` of kind `k`: what trial deletion does to a child `c`.
    /// A kind that can't be in a cycle isn't looked into: only a white
    /// object's drop of it.
    fn step_body(&self, k: &DropKey) -> Body {
        let (c, op, h) = (0, 1, 2);
        let n = self.number(k) as i32;
        let mut code = skip_uncounted(c, h);
        if !self.cyclic[n as usize] {
            code.extend([
                I::LocalGet(op),
                I::I32Const(OP_WHITE),
                I::I32Eq,
                I::If(BlockType::Empty),
                I::LocalGet(c),
                I::ReturnCall(self.drop(k, false)),
                I::End,
                I::End,
            ]);
            return Body {
                locals: vec![ValType::I32],
                code,
            };
        }
        let is = |o: i32| {
            [
                I::LocalGet(op),
                I::I32Const(o),
                I::I32Eq,
                I::If(BlockType::Empty),
            ]
        };
        let push = [I::LocalGet(c), I::I32Const(n), I::ReturnCall(self.push())];
        // Gray: one count less, for the reference from inside.
        code.extend(is(OP_GRAY));
        code.extend([
            I::LocalGet(c),
            I::LocalGet(h),
            I::I32Const(1),
            I::I32Sub,
            store(Storage::I32, 0),
        ]);
        code.extend(push.clone());
        code.push(I::End);
        // Black: the count back, and black, if it isn't, to be looked into.
        code.extend(is(OP_BLACK));
        code.extend([
            I::LocalGet(c),
            I::LocalGet(h),
            I::I32Const(1),
            I::I32Add,
            I::I32Const(!COLOR),
            I::I32And,
            store(Storage::I32, 0),
            I::LocalGet(h),
            I::I32Const(COLOR),
            I::I32And,
            I::I32Eqz,
            I::If(BlockType::Empty),
            I::Return,
            I::End,
        ]);
        code.extend(push.clone());
        code.push(I::End);
        // Scan and white: looked into in turn.
        code.extend(push);
        code.push(I::End);
        Body {
            locals: vec![ValType::I32],
            code,
        }
    }

    /// `children(p, op)` of kind `k`: the step `op` on each child, with
    /// what a field of type `t` holds stepped as its kind if it can be in
    /// a cycle and `t` isn't `imm`, and otherwise only dropped, for white.
    /// White and free give the object's memory back too, free only that.
    fn children_body(&self, cx: &mut Cx<'_>, k: &DropKey, two: u32, slots: Slots) -> Body {
        let (p, op) = (0, 1);
        let mut code = Vec::new();
        let mut locals = Vec::new();
        // The step on the child at `at` from address `base` in local `b`,
        // `imm` if its type is.
        let child = |code: &mut Vec<I<'static>>, b: u32, at: u32, imm: bool, k: &DropKey| {
            let cyclic = self.cyclic[self.number(k) as usize] && !imm;
            if cyclic {
                code.extend([
                    I::LocalGet(b),
                    load(Storage::I32, at),
                    I::LocalGet(op),
                    I::Call(self.step(k)),
                ]);
            } else {
                code.extend([
                    I::LocalGet(op),
                    I::I32Const(OP_WHITE),
                    I::I32Eq,
                    I::If(BlockType::Empty),
                    I::LocalGet(b),
                    load(Storage::I32, at),
                    I::Call(self.drop(k, imm)),
                    I::End,
                ]);
            }
        };
        // Gives the memory back, `size` from what's on the stack, for white
        // or free.
        let release = |code: &mut Vec<I<'static>>, size: Vec<I<'static>>| {
            code.extend([
                I::LocalGet(op),
                I::I32Const(OP_WHITE),
                I::I32GeU,
                I::If(BlockType::Empty),
                I::LocalGet(p),
            ]);
            code.extend(size);
            code.extend([I::Call(self.release()), I::End]);
        };
        let free_only = [
            I::LocalGet(op),
            I::I32Const(OP_FREE),
            I::I32Eq,
            I::I32Eqz,
            I::If(BlockType::Empty),
        ];
        match k {
            DropKey::Data(d) => {
                let ctors = cx.m.data.get(d).map_or(&[][..], |d| &d.ctors[..]);
                for (i, c) in ctors.iter().enumerate() {
                    let (offsets, size) = layout(&c.fields);
                    code.extend([
                        I::LocalGet(p),
                        load(Storage::I32, 4),
                        I::I32Const(i as i32),
                        I::I32Eq,
                        I::If(BlockType::Empty),
                    ]);
                    code.extend(free_only.clone());
                    for (t, &(at, _)) in c.fields.iter().zip(&offsets) {
                        if let Some(k) = key_of(t) {
                            child(&mut code, p, at, is_imm(t), &k);
                        }
                    }
                    code.push(I::End);
                    release(&mut code, vec![I::I32Const(size as i32)]);
                    code.extend([I::Return, I::End]);
                }
            }
            DropKey::Array(s, elems, elems_imm) => {
                let (n, at) = (2, 3);
                locals.extend([ValType::I32, ValType::I32]);
                code.extend([I::LocalGet(p), load(Storage::I32, 4), I::LocalSet(n)]);
                if let Some(e) = elems {
                    code.extend(free_only.clone());
                    code.extend([
                        I::LocalGet(p),
                        I::LocalSet(at),
                        I::Block(BlockType::Empty),
                        I::Loop(BlockType::Empty),
                        I::LocalGet(at),
                        I::LocalGet(p),
                        I::I32Sub,
                        I::LocalGet(n),
                        I::I32Const(2),
                        I::I32Shl,
                        I::I32GeU,
                        I::BrIf(1),
                    ]);
                    child(&mut code, at, 8, *elems_imm, e);
                    code.extend([
                        I::LocalGet(at),
                        I::I32Const(4),
                        I::I32Add,
                        I::LocalSet(at),
                        I::Br(0),
                        I::End,
                        I::End,
                        I::End,
                    ]);
                }
                release(
                    &mut code,
                    vec![
                        I::LocalGet(n),
                        I::I32Const(s.size().trailing_zeros() as i32),
                        I::I32Shl,
                        I::I32Const(8),
                        I::I32Add,
                    ],
                );
            }
            DropKey::Ref(_, inner, inner_imm) => {
                if let Some(k) = inner {
                    code.extend(free_only.clone());
                    child(&mut code, p, 8, *inner_imm, k);
                    code.push(I::End);
                }
                release(&mut code, vec![I::I32Const(16)]);
            }
            DropKey::Closure | DropKey::Cont => code.extend([
                I::LocalGet(p),
                I::LocalGet(op),
                I::LocalGet(p),
                load(Storage::I32, 4),
                I::I32Const(if *k == DropKey::Cont {
                    slots.kont_children
                } else {
                    slots.closure_children
                }),
                I::I32Add,
                I::ReturnCallIndirect {
                    type_index: two,
                    table_index: 0,
                },
            ]),
            // It's never white, since it's never in a cycle, but if the
            // collector frees it, its slot goes too.
            DropKey::Host => {
                code.extend([
                    I::LocalGet(op),
                    I::I32Const(OP_WHITE),
                    I::I32GeU,
                    I::If(BlockType::Empty),
                ]);
                code.extend(recycle_host(cx, p));
                code.push(I::End);
            }
            DropKey::Any => {
                code.extend(free_only.clone());
                code.extend([
                    I::LocalGet(p),
                    load(Storage::I32, 4),
                    I::If(BlockType::Empty),
                    I::LocalGet(p),
                    load(Storage::I32, 8),
                    I::LocalGet(op),
                    I::LocalGet(p),
                    load(Storage::I32, 4),
                    I::I32Const(slots.kind_step - 1),
                    I::I32Add,
                    I::CallIndirect {
                        type_index: two,
                        table_index: 0,
                    },
                    I::End,
                    I::End,
                ]);
                release(&mut code, vec![I::I32Const(16)]);
            }
        }
        code.push(I::End);
        Body { locals, code }
    }

    /// The function that frees a closure of `f` with `n` captures, and
    /// drops them.
    pub fn closure_free(&self, cx: &Cx<'_>, f: FuncId, n: usize) -> Body {
        let tys = &cx.m.funcs[&f].sig().params[..n];
        let (offsets, size) = layout(tys);
        let fields: Vec<(u32, Ty)> = (offsets.iter().zip(tys))
            .map(|(&(at, _), t)| (at, t.clone()))
            .collect();
        self.free_fields(&fields, size, Vec::new(), Vec::new())
    }

    /// The function that frees an object of `size` bytes and drops its
    /// `fields`, at their offsets, after running `first`, which can use
    /// `locals` from local 1.
    pub fn free_fields(
        &self,
        fields: &[(u32, Ty)],
        size: u32,
        first: Vec<I<'static>>,
        mut locals: Vec<ValType>,
    ) -> Body {
        let held: Vec<(u32, u32)> = (fields.iter())
            .filter_map(|(at, t)| self.drop_of(t).map(|d| (*at, d)))
            .collect();
        let mut code = first;
        let base = 1 + locals.len() as u32;
        for (j, (at, _)) in held.iter().enumerate() {
            locals.push(ValType::I32);
            code.extend([
                I::LocalGet(0),
                load(Storage::I32, *at),
                I::LocalSet(base + j as u32),
            ]);
        }
        code.extend([
            I::LocalGet(0),
            I::I32Const(size as i32),
            I::Call(self.release_obj()),
        ]);
        for (j, (_, d)) in held.iter().enumerate() {
            code.push(I::LocalGet(base + j as u32));
            code.push(if j + 1 == held.len() {
                I::ReturnCall(*d)
            } else {
                I::Call(*d)
            });
        }
        code.push(I::End);
        Body { locals, code }
    }

    /// `children(p, op)` of a closure of `f` with `n` captures.
    pub fn closure_children(&self, cx: &Cx<'_>, f: FuncId, n: usize) -> Body {
        let tys = &cx.m.funcs[&f].sig().params[..n];
        let (offsets, size) = layout(tys);
        let fields: Vec<(u32, Ty)> = (offsets.iter().zip(tys))
            .map(|(&(at, _), t)| (at, t.clone()))
            .collect();
        self.children_of_fields(&fields, size, Vec::new(), Vec::new())
    }

    /// `children(p, op)` of an object of `size` bytes with `fields`, at
    /// their offsets, which runs `freeing` before it gives the memory
    /// back, for white or free; `freeing` can use `locals` from local 2.
    pub fn children_of_fields(
        &self,
        fields: &[(u32, Ty)],
        size: u32,
        freeing: Vec<I<'static>>,
        locals: Vec<ValType>,
    ) -> Body {
        let (p, op) = (0, 1);
        let mut code = vec![
            I::LocalGet(op),
            I::I32Const(OP_FREE),
            I::I32Ne,
            I::If(BlockType::Empty),
        ];
        for (at, t) in fields {
            let at = *at;
            let Some(k) = key_of(t) else { continue };
            if self.cyclic[self.number(&k) as usize] && !is_imm(t) {
                code.extend([
                    I::LocalGet(p),
                    load(Storage::I32, at),
                    I::LocalGet(op),
                    I::Call(self.step(&k)),
                ]);
            } else {
                code.extend([
                    I::LocalGet(op),
                    I::I32Const(OP_WHITE),
                    I::I32Eq,
                    I::If(BlockType::Empty),
                    I::LocalGet(p),
                    load(Storage::I32, at),
                    I::Call(self.drop(&k, is_imm(t))),
                    I::End,
                ]);
            }
        }
        code.extend([
            I::End,
            I::LocalGet(op),
            I::I32Const(OP_WHITE),
            I::I32GeU,
            I::If(BlockType::Empty),
        ]);
        code.extend(freeing);
        code.extend([
            I::LocalGet(p),
            I::I32Const(size as i32),
            I::Call(self.release()),
            I::End,
            I::End,
        ]);
        Body { locals, code }
    }
}

fn sig(params: &[ValType], results: &[ValType]) -> crate::FnSig {
    crate::FnSig {
        params: params.to_vec(),
        results: results.to_vec(),
    }
}

/// Whether the color of the object in local `p` is `c`; leaves its header
/// in local `h`.
fn color_is(p: u32, h: u32, c: i32) -> Vec<I<'static>> {
    vec![
        I::LocalGet(p),
        load(Storage::I32, 0),
        I::LocalTee(h),
        I::I32Const(COLOR),
        I::I32And,
        I::I32Const(c),
        I::I32Eq,
    ]
}

/// Sets the color of the object in local `p`, whose header is in local `h`.
fn set_color(p: u32, h: u32, c: i32) -> Vec<I<'static>> {
    vec![
        I::LocalGet(p),
        I::LocalGet(h),
        I::I32Const(!COLOR),
        I::I32And,
        I::I32Const(c),
        I::I32Or,
        store(Storage::I32, 0),
    ]
}

/// `children(p, op)` of the object in local `p`, of the kind numbered in
/// local `kind`, through the table.
fn children_call(p: u32, kind: u32, op: i32, two: u32, slots: Slots) -> Vec<I<'static>> {
    vec![
        I::LocalGet(p),
        I::I32Const(op),
        I::LocalGet(kind),
        I::I32Const(slots.kind_children),
        I::I32Add,
        I::CallIndirect {
            type_index: two,
            table_index: 0,
        },
    ]
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
            I::I32Const(COUNTED),
            I::I32And,
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

/// Lets go of the host value of the box in local `p`, whose count is 0:
/// clears its slot, so the host can collect what it held, and puts the box
/// on the free list, slot and all, for the next host value. Its memory
/// stays a box, so it no longer counts as live.
fn recycle_host(cx: &Cx<'_>, p: u32) -> Vec<I<'static>> {
    let Some(hosts) = &cx.hosts else {
        return vec![I::Unreachable];
    };
    let free = hosts.free.expect("a free list of host boxes, with counts");
    vec![
        I::LocalGet(p),
        load(Storage::I32, 8),
        I::RefNull(wasm_encoder::HeapType::EXTERN),
        I::TableSet(hosts.table),
        I::LocalGet(p),
        I::GlobalGet(free),
        store(Storage::I32, 4),
        I::LocalGet(p),
        I::GlobalSet(free),
        I::GlobalGet(LIVE),
        I::I32Const(1),
        I::I32Sub,
        I::GlobalSet(LIVE),
    ]
}
