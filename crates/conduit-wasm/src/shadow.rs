//! The shadow stack's functions, and the code that pushes on it and pops
//! what was pushed: see [`ShadowStack`].

use wasm_encoder::{BlockType, Instruction as I, ValType};

use crate::counts::LIVE;
use crate::func::{Body, load, store};
use crate::{Cx, FnSig, HEADER, ShadowStack, Storage};

/// The bodies of `grow` and `release`, in that order, with their types.
pub(crate) fn bodies(cx: &mut Cx<'_>, stack: ShadowStack) -> Vec<(u32, Body)> {
    let one = cx.sig(FnSig::new([ValType::I32], []));
    let two = cx.sig(FnSig::new([ValType::I32, ValType::I32], []));
    vec![(one, grow(cx, stack)), (two, release(stack))]
}

/// Takes a segment's memory from the heap, which isn't an object.
fn from_heap(cx: &Cx<'_>, size: Vec<I<'static>>, into: u32) -> Vec<I<'static>> {
    let mut code = size;
    code.extend([I::Call(cx.alloc), I::LocalSet(into)]);
    if cx.rt.is_some() {
        code.extend([
            I::GlobalGet(LIVE),
            I::I32Const(1),
            I::I32Sub,
            I::GlobalSet(LIVE),
        ]);
    }
    code
}

/// `grow(n)`: chains a segment that `n` bytes fit in, after its header:
/// the first free one, if they fit in it, or else a new one, of the usual
/// size if they fit in that, or just big enough.
fn grow(cx: &Cx<'_>, stack: ShadowStack) -> Body {
    let (n, size, seg) = (0, 1, 2);
    let usual = stack.segment as i32;
    let mut code = vec![
        I::LocalGet(n),
        I::I32Const(HEADER as i32 + 7),
        I::I32Add,
        I::I32Const(-8),
        I::I32And,
        I::LocalTee(size),
        I::I32Const(usual),
        I::LocalGet(size),
        I::I32Const(usual),
        I::I32GtU,
        I::Select,
        I::LocalSet(size),
        I::GlobalGet(stack.free),
        I::LocalTee(seg),
        I::If(BlockType::Result(ValType::I32)),
        I::LocalGet(seg),
        load(Storage::I32, 8),
        I::LocalGet(size),
        I::I32GeU,
        I::Else,
        I::I32Const(0),
        I::End,
        I::If(BlockType::Empty),
        I::LocalGet(seg),
        load(Storage::I32, 0),
        I::GlobalSet(stack.free),
        I::LocalGet(seg),
        load(Storage::I32, 8),
        I::LocalSet(size),
        I::Else,
    ];
    code.extend(from_heap(cx, vec![I::LocalGet(size)], seg));
    code.extend([
        I::End,
        I::LocalGet(seg),
        I::GlobalGet(stack.sp),
        store(Storage::I32, 0),
        I::LocalGet(seg),
        I::GlobalGet(stack.limit),
        store(Storage::I32, 4),
        I::LocalGet(seg),
        I::LocalGet(size),
        store(Storage::I32, 8),
        I::LocalGet(seg),
        I::I32Const(HEADER as i32),
        I::I32Add,
        I::GlobalSet(stack.limit),
        I::LocalGet(seg),
        I::LocalGet(size),
        I::I32Add,
        I::GlobalSet(stack.sp),
        I::End,
    ]);
    Body {
        locals: vec![ValType::I32, ValType::I32],
        code,
    }
}

/// `release(from, to)`: puts the segment whose limit is `from`, and the
/// ones before it, until the one whose limit is `to`, on the free list.
/// Segments never go back to the heap, so the shadow stack takes as much
/// memory as it's ever been deep.
fn release(stack: ShadowStack) -> Body {
    let (from, to, seg) = (0, 1, 2);
    let mut code = vec![
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(from),
        I::LocalGet(to),
        I::I32Eq,
        I::BrIf(1),
        I::LocalGet(from),
        I::I32Const(HEADER as i32),
        I::I32Sub,
        I::LocalTee(seg),
        load(Storage::I32, 4),
        I::LocalSet(from),
    ];
    code.extend(free_segment(stack, seg));
    code.extend([I::Br(0), I::End, I::End, I::End]);
    Body {
        locals: vec![ValType::I32],
        code,
    }
}

/// Puts the segment in local `seg` on the free list.
pub(crate) fn free_segment(stack: ShadowStack, seg: u32) -> Vec<I<'static>> {
    vec![
        I::LocalGet(seg),
        I::GlobalGet(stack.free),
        store(Storage::I32, 0),
        I::LocalGet(seg),
        I::GlobalSet(stack.free),
    ]
}

/// Pushes the bytes that `n` gives, a multiple of 8, which it may run more
/// than once, and leaves their address in local `at`.
pub(crate) fn push(stack: ShadowStack, n: &[I<'static>], at: u32) -> Vec<I<'static>> {
    let mut code = vec![I::GlobalGet(stack.sp), I::GlobalGet(stack.limit), I::I32Sub];
    code.extend(n.iter().cloned());
    code.extend([I::I32LtU, I::If(BlockType::Empty)]);
    code.extend(n.iter().cloned());
    code.extend([I::Call(stack.grow), I::End, I::GlobalGet(stack.sp)]);
    code.extend(n.iter().cloned());
    code.extend([I::I32Sub, I::LocalTee(at), I::GlobalSet(stack.sp)]);
    code
}

/// Saves the pointer and limit into locals `sp` and `limit`.
pub(crate) fn save(stack: ShadowStack, sp: u32, limit: u32) -> [I<'static>; 4] {
    [
        I::GlobalGet(stack.sp),
        I::LocalSet(sp),
        I::GlobalGet(stack.limit),
        I::LocalSet(limit),
    ]
}

/// Pops back to the pointer and limit in locals `sp` and `limit`, giving
/// back the segments chained since.
pub(crate) fn restore(stack: ShadowStack, sp: u32, limit: u32) -> Vec<I<'static>> {
    vec![
        I::GlobalGet(stack.limit),
        I::LocalGet(limit),
        I::I32Ne,
        I::If(BlockType::Empty),
        I::GlobalGet(stack.limit),
        I::LocalGet(limit),
        I::Call(stack.release),
        I::LocalGet(limit),
        I::GlobalSet(stack.limit),
        I::End,
        I::LocalGet(sp),
        I::GlobalSet(stack.sp),
    ]
}
