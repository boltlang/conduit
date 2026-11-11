//! Where objects' memory comes from: free lists by size, in front of the
//! system's `malloc`, as the Wasm backend's are (#42).
//!
//! A block of memory is the object, which starts with its header, and,
//! the 8 bytes before it, the block's size class: its size in 8-byte
//! words, from 1 to [`SMALL`], or 0 for one bigger than that. Freeing
//! needs to know it, and `conduit_dealloc`, which frees any object, from
//! `free`, a drop, or the cycle collector, only has the address.
//!
//! - A **small** block comes from the free list of its class if there's
//!   one on it, or else is bumped from a chunk of [`CHUNK`] bytes that
//!   `malloc` gives. Freed, it goes on the list of its class, linked
//!   through its first word. Chunks are never given back: what's freed is
//!   used again for objects of its size.
//! - A **large** block comes from `malloc` alone, and goes back to `free`.
//!
//! Memory isn't zeroed: an object's fields are all set when it's made, and
//! `array_alloc`, whose elements are null until they're set, zeroes them
//! itself. Without counts, nothing is freed, and every small object is
//! bumped.

/// The biggest small block, in 8-byte words: 256 bytes.
pub(crate) const SMALL: u64 = 32;

/// The bytes of a chunk small blocks are bumped from.
pub(crate) const CHUNK: u64 = 1 << 20;

/// The heap's globals and `conduit_alloc`, in LLVM's text.
pub(crate) fn runtime() -> String {
    format!(
        r#"
@conduit_lists = internal global [{n} x ptr] zeroinitializer
@conduit_bump = internal global ptr null
@conduit_bump_end = internal global ptr null

declare ptr @malloc(i64)

; A block for an object of %size bytes, which the caller sets every byte
; of that's read. Traps if there's no memory left.
define internal ptr @conduit_alloc(i64 %size) {{
entry:
  %r7 = add i64 %size, 7
  %words = lshr i64 %r7, 3
  %bytes = shl i64 %words, 3
  %block = add i64 %bytes, 8
  %small = icmp ule i64 %words, {small}
  br i1 %small, label %list, label %large
list:
  %slot = getelementptr [{n} x ptr], ptr @conduit_lists, i64 0, i64 %words
  %head = load ptr, ptr %slot
  %empty = icmp eq ptr %head, null
  br i1 %empty, label %bump, label %pop
pop:
  %next = load ptr, ptr %head
  store ptr %next, ptr %slot
  ret ptr %head
bump:
  %cur = load ptr, ptr @conduit_bump
  %end = load ptr, ptr @conduit_bump_end
  %ci = ptrtoint ptr %cur to i64
  %ei = ptrtoint ptr %end to i64
  %room = sub i64 %ei, %ci
  %fits = icmp uge i64 %room, %block
  br i1 %fits, label %carve, label %refill
refill:
  %chunk = call ptr @malloc(i64 {chunk})
  %none = icmp eq ptr %chunk, null
  br i1 %none, label %out, label %got
got:
  %chunk_end = getelementptr i8, ptr %chunk, i64 {chunk}
  store ptr %chunk_end, ptr @conduit_bump_end
  br label %carve
carve:
  %base = phi ptr [ %cur, %bump ], [ %chunk, %got ]
  %after = getelementptr i8, ptr %base, i64 %block
  store ptr %after, ptr @conduit_bump
  store i64 %words, ptr %base
  %p = getelementptr i8, ptr %base, i64 8
  ret ptr %p
large:
  %q = call ptr @malloc(i64 %block)
  %lnone = icmp eq ptr %q, null
  br i1 %lnone, label %out, label %lgot
lgot:
  store i64 0, ptr %q
  %lp = getelementptr i8, ptr %q, i64 8
  ret ptr %lp
out:
  call void @llvm.trap()
  unreachable
}}
"#,
        n = SMALL + 1,
        small = SMALL,
        chunk = CHUNK,
    )
}

/// `conduit_dealloc(p)`, which gives back the block of object `p`, in
/// LLVM's text, for the counting runtime, which counts it as freed.
pub(crate) fn dealloc() -> String {
    format!(
        r#"
; Frees an object's memory, without dropping what it holds: on the free
; list of its class, or back to `free` if it's large.
define tailcc void @conduit_dealloc(ptr %p) {{
entry:
  %at = getelementptr i8, ptr %p, i64 -8
  %words = load i64, ptr %at
  %large = icmp eq i64 %words, 0
  br i1 %large, label %back, label %push
back:
  call void @free(ptr %at)
  br label %done
push:
  %slot = getelementptr [{n} x ptr], ptr @conduit_lists, i64 0, i64 %words
  %head = load ptr, ptr %slot
  store ptr %head, ptr %p
  store ptr %p, ptr %slot
  br label %done
done:
  %n = load i64, ptr @conduit_live
  %n1 = sub i64 %n, 1
  store i64 %n1, ptr @conduit_live
  ret void
}}
"#,
        n = SMALL + 1,
    )
}
