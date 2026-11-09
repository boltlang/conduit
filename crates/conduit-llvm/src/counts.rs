//! Reference counting, for a module whose counts are explicit, as
//! `conduit_opt::ownership::insert_counts` makes them, when
//! [`Options::counts`](crate::Options::counts) asks for it, and cycle
//! collection, when [`Options::cycles`](crate::Options::cycles) does too.
//!
//! - **The header word** at 0 is the count, in bits 0 to 27, then the
//!   collector's color in bits 28 and 29, whether the object is buffered
//!   as a possible root of a cycle in bit 30, and in bit 31 whether it's
//!   immortal, as a constant is, which counting skips. A count that would
//!   pass its 28 bits saturates instead: the object turns immortal and
//!   leaks, rather than being freed while it's still in use, as in the
//!   Wasm backend and the evaluator.
//! - **Dropping** goes through a function for each kind of object, a
//!   [`Kind`]: a data type, arrays and refs of what they hold, closures and
//!   `any`. At 0, it frees the object and drops what it held, tail-calling
//!   the drop of its last field, so that dropping a list is a loop. A
//!   closure holds, at 16, a descriptor of the function that frees it and
//!   drops its captures and of its visit function; an `any`, at 16, the
//!   descriptor of the kind of what it holds, its drop and visit, or null.
//! - **Cycles** are collected by trial deletion (Bacon and Rajan,
//!   synchronous), as in the Wasm backend. A `drop` or `decref` that
//!   leaves an object above 0 buffers it, unless its type is `imm`: each
//!   kind has a second drop for those, since an `imm` object can't be in a
//!   cycle. A buffered object's memory only goes back through the
//!   collector, so the buffer never points at freed memory. The buffer
//!   holds 256 objects, and is collected when it's full, or when
//!   `conduit_collect` asks. Each kind's visit function does a step of the
//!   collector on each object an object holds, which pushes it on a stack
//!   on the heap, so the collector doesn't recurse. What's garbage is
//!   freed at the end, so that the collector never reads freed memory.
//! - **`@conduit_live`** counts the objects allocated and not yet freed, for
//!   tests to compare with the evaluator's.

use std::collections::BTreeSet;
use std::fmt::Write;

use conduit_ir::{DataId, Module, Mutability, Ty};

use crate::{ctor_fields, layout};

/// A kind of object that's dropped the same way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    Data(DataId),
    /// What its elements are, if they're counted.
    Array(Option<Box<Kind>>),
    Ref(Option<Box<Kind>>),
    Closure,
    Any,
}

impl Kind {
    /// How a value of type `ty` is dropped, or `None` if it isn't counted.
    pub fn of(t: &Ty) -> Option<Kind> {
        Some(match t {
            Ty::Data(d, ..) => Kind::Data(*d),
            Ty::Array(e, _) => Kind::Array(Kind::of(e).map(Box::new)),
            Ty::Ref(t) => Kind::Ref(Kind::of(t).map(Box::new)),
            Ty::Fn(..) => Kind::Closure,
            Ty::Any(_) => Kind::Any,
            _ => return None,
        })
    }

    fn name(&self) -> String {
        match self {
            Kind::Data(d) => format!("data{}", d.0),
            Kind::Array(e) => format!("array.{}", e.as_ref().map_or("_".into(), |e| e.name())),
            Kind::Ref(e) => format!("ref.{}", e.as_ref().map_or("_".into(), |e| e.name())),
            Kind::Closure => "closure".into(),
            Kind::Any => "any".into(),
        }
    }

    /// The function that drops an object of this kind, which may buffer
    /// it as a possible root of a cycle.
    pub fn drop_fn(&self) -> String {
        format!("@conduit_drop.{}", self.name())
    }

    /// The function that drops an object of this kind whose type is
    /// `imm`, which never buffers it. Only with cycles.
    pub fn drop_imm_fn(&self) -> String {
        format!("@conduit_drop_imm.{}", self.name())
    }

    /// The function that does a step of the collector on each object an
    /// object of this kind holds. Only with cycles.
    pub fn visit_fn(&self) -> String {
        format!("@conduit_visit.{}", self.name())
    }

    /// The descriptor an `any` holding this kind points to: its drop and
    /// visit functions.
    pub fn desc(&self) -> String {
        format!("@conduit_kind.{}", self.name())
    }

    /// The kinds of what an object of this kind holds, which a closure's
    /// and an `any`'s say for themselves.
    fn held(&self, m: &Module) -> Vec<Kind> {
        match self {
            Kind::Data(d) => (m.data.get(d).into_iter())
                .flat_map(|d| &d.ctors)
                .flat_map(|c| &c.fields)
                .filter_map(Kind::of)
                .collect(),
            Kind::Array(Some(e)) | Kind::Ref(Some(e)) => vec![(**e).clone()],
            _ => Vec::new(),
        }
    }
}

/// Whether a value of type `t` is `imm`, so its object can't be in a cycle.
pub(crate) fn is_imm(t: &Ty) -> bool {
    t.mutability() == Some(Mutability::Imm)
}

/// The runtime every counted module has: `dup`, `add`, `decref`, `release`,
/// the counter of live objects, and with `cycles`, the collector.
pub(crate) fn runtime(cycles: bool) -> String {
    let mut s = String::from(COMMON);
    s.push_str(if cycles { CYCLES } else { NO_CYCLES });
    s
}

/// Header bits, as `i32`s in the LLVM text below:
/// - count `0x0fff_ffff` = 268435455
/// - color `0x3000_0000` = 805306368: gray 268435456, white 536870912,
///   purple 805306368, black 0
/// - buffered `0x4000_0000` = 1073741824
/// - count and immortal `0x8fff_ffff` = -1879048193
/// - all but the count `0x7000_0000` = 1879048192
/// - all but the color `!0x3000_0000` = -805306369
/// - all but buffered `!0x4000_0000` = -1073741825
const COMMON: &str = r#"
@conduit_live = global i64 0

; Frees an object's memory, without dropping what it holds.
define tailcc void @conduit_release(ptr %p) {
  call void @free(ptr %p)
  %n = load i64, ptr @conduit_live
  %n1 = sub i64 %n, 1
  store i64 %n1, ptr @conduit_live
  ret void
}

; Adds %k to the count of %p, unless it's null or immortal, saturating:
; past 0x0fffffff, the object turns immortal.
define tailcc void @conduit_add(ptr %p, i32 %k) {
entry:
  %null = icmp eq ptr %p, null
  br i1 %null, label %done, label %some
some:
  %h = load i32, ptr %p
  %immortal = icmp slt i32 %h, 0
  br i1 %immortal, label %done, label %count
count:
  %c = and i32 %h, 268435455
  %c64 = zext i32 %c to i64
  %k64 = zext i32 %k to i64
  %sum = add i64 %c64, %k64
  %over = icmp ugt i64 %sum, 268435455
  %s32 = trunc i64 %sum to i32
  %rest = and i32 %h, 1879048192
  %kept = or i32 %rest, %s32
  %sat = or i32 %rest, -2147483648
  %new = select i1 %over, i32 %sat, i32 %kept
  store i32 %new, ptr %p
  br label %done
done:
  ret void
}

define tailcc void @conduit_dup(ptr %p) {
  tail call tailcc void @conduit_add(ptr %p, i32 1)
  ret void
}

; One count less, of an object that's shared, so it's never freed.
define tailcc void @conduit_decref(ptr %p) {
entry:
  %h = load i32, ptr %p
  %counted = icmp sgt i32 %h, 0
  br i1 %counted, label %dec, label %done
dec:
  %h1 = sub i32 %h, 1
  store i32 %h1, ptr %p
  br label %done
done:
  ret void
}

; Whether %p's count is 1.
define tailcc i32 @conduit_is_unique(ptr %p) {
entry:
  %null = icmp eq ptr %p, null
  br i1 %null, label %no, label %some
some:
  %h = load i32, ptr %p
  %c = and i32 %h, -1879048193
  %one = icmp eq i32 %c, 1
  %r = zext i1 %one to i32
  ret i32 %r
no:
  ret i32 0
}

; Takes one from %p's count, and gives 1 if that was the last, for the
; caller to free it: 0 if it's null, immortal or still shared.
define tailcc i1 @conduit_last(ptr %p) {
entry:
  %null = icmp eq ptr %p, null
  br i1 %null, label %no, label %some
some:
  %h = load i32, ptr %p
  %c = and i32 %h, -1879048193
  %one = icmp eq i32 %c, 1
  br i1 %one, label %yes, label %shared
shared:
  %counted = icmp sgt i32 %h, 0
  br i1 %counted, label %dec, label %no
dec:
  %h1 = sub i32 %h, 1
  store i32 %h1, ptr %p
  br label %no
yes:
  ret i1 1
no:
  ret i1 0
}
"#;

const NO_CYCLES: &str = r#"
; Frees an object whose count got to 0.
define tailcc void @conduit_free(ptr %p) {
  tail call tailcc void @conduit_release(ptr %p)
  ret void
}

; The memory of an object whose count got to 0, for `reuse`.
define tailcc ptr @conduit_take(ptr %p) {
  ret ptr %p
}

define void @conduit_collect() {
  ret void
}
"#;

const CYCLES: &str = r#"
declare ptr @realloc(ptr, i64)

; The possible roots of cycles, with their visit functions.
@conduit_roots = global [256 x { ptr, ptr }] zeroinitializer
@conduit_nroots = global i32 0

; The collector's stack: what to do a step of, with its visit function
; and the step.
@conduit_stack = global ptr null
@conduit_cap = global i64 0
@conduit_top = global i64 0

; The garbage found, to free once the collector is done with it.
@conduit_dead = global ptr null
@conduit_dead_cap = global i64 0
@conduit_dead_top = global i64 0

; Frees an object whose count got to 0, unless it's buffered: then the
; collector frees it, with its count at 0 and black.
define tailcc void @conduit_free(ptr %p) {
entry:
  %h = load i32, ptr %p
  %b = and i32 %h, 1073741824
  %buffered = icmp ne i32 %b, 0
  br i1 %buffered, label %keep, label %go
keep:
  store i32 1073741824, ptr %p
  ret void
go:
  tail call tailcc void @conduit_release(ptr %p)
  ret void
}

; The memory of an object whose count got to 0, for `reuse`, or null if
; it's buffered: then the collector frees it.
define tailcc ptr @conduit_take(ptr %p) {
entry:
  %h = load i32, ptr %p
  %b = and i32 %h, 1073741824
  %buffered = icmp ne i32 %b, 0
  br i1 %buffered, label %keep, label %go
keep:
  store i32 1073741824, ptr %p
  ret ptr null
go:
  ret ptr %p
}

; Makes %p, whose header is now %h, a possible root: purple, and in the
; buffer if it isn't yet, which is collected when it's full.
define tailcc void @conduit_possible_root(ptr %p, ptr %v, i32 %h) {
entry:
  %uncolored = and i32 %h, -805306369
  %purple = or i32 %uncolored, 805306368
  %b = and i32 %h, 1073741824
  %buffered = icmp ne i32 %b, 0
  br i1 %buffered, label %already, label %add
already:
  store i32 %purple, ptr %p
  ret void
add:
  %marked = or i32 %purple, 1073741824
  store i32 %marked, ptr %p
  %n = load i32, ptr @conduit_nroots
  %n64 = zext i32 %n to i64
  %slot = getelementptr [256 x { ptr, ptr }], ptr @conduit_roots, i64 0, i64 %n64
  store ptr %p, ptr %slot
  %vs = getelementptr { ptr, ptr }, ptr %slot, i64 0, i32 1
  store ptr %v, ptr %vs
  %n1 = add i32 %n, 1
  store i32 %n1, ptr @conduit_nroots
  %full = icmp eq i32 %n1, 256
  br i1 %full, label %collect, label %done
collect:
  tail call tailcc void @conduit_collect_cycles()
  ret void
done:
  ret void
}

; `decref` of an object whose type isn't `imm`, which may be a root.
define tailcc void @conduit_decref_buf(ptr %p, ptr %v) {
entry:
  %h = load i32, ptr %p
  %counted = icmp sgt i32 %h, 0
  br i1 %counted, label %dec, label %done
dec:
  %h1 = sub i32 %h, 1
  tail call tailcc void @conduit_possible_root(ptr %p, ptr %v, i32 %h1)
  ret void
done:
  ret void
}

; `conduit_last` of an object whose type isn't `imm`: one that's left
; shared may be a root.
define tailcc i1 @conduit_last_buf(ptr %p, ptr %v) {
entry:
  %null = icmp eq ptr %p, null
  br i1 %null, label %no, label %some
some:
  %h = load i32, ptr %p
  %c = and i32 %h, -1879048193
  %one = icmp eq i32 %c, 1
  br i1 %one, label %yes, label %shared
shared:
  %counted = icmp sgt i32 %h, 0
  br i1 %counted, label %dec, label %no
dec:
  %h1 = sub i32 %h, 1
  call tailcc void @conduit_possible_root(ptr %p, ptr %v, i32 %h1)
  br label %no
yes:
  ret i1 1
no:
  ret i1 0
}

; Pushes a step of %op on %p, whose visit function is %v, on the stack,
; which doubles when it's full.
define tailcc void @conduit_push(ptr %p, ptr %v, i32 %op) {
entry:
  %top = load i64, ptr @conduit_top
  %cap = load i64, ptr @conduit_cap
  %full = icmp eq i64 %top, %cap
  br i1 %full, label %grow, label %put
grow:
  %twice = mul i64 %cap, 2
  %small = icmp ult i64 %twice, 64
  %cap1 = select i1 %small, i64 64, i64 %twice
  %bytes = mul i64 %cap1, 24
  %old = load ptr, ptr @conduit_stack
  %new = call ptr @realloc(ptr %old, i64 %bytes)
  %failed = icmp eq ptr %new, null
  br i1 %failed, label %oom, label %grown
oom:
  call void @llvm.trap()
  unreachable
grown:
  store ptr %new, ptr @conduit_stack
  store i64 %cap1, ptr @conduit_cap
  br label %put
put:
  %s = load ptr, ptr @conduit_stack
  %e = getelementptr { ptr, ptr, i32 }, ptr %s, i64 %top
  store ptr %p, ptr %e
  %ve = getelementptr { ptr, ptr, i32 }, ptr %e, i64 0, i32 1
  store ptr %v, ptr %ve
  %oe = getelementptr { ptr, ptr, i32 }, ptr %e, i64 0, i32 2
  store i32 %op, ptr %oe
  %top1 = add i64 %top, 1
  store i64 %top1, ptr @conduit_top
  ret void
}

; Puts %p, which is garbage, on the list of what to free at the end.
define tailcc void @conduit_bury(ptr %p) {
entry:
  %top = load i64, ptr @conduit_dead_top
  %cap = load i64, ptr @conduit_dead_cap
  %full = icmp eq i64 %top, %cap
  br i1 %full, label %grow, label %put
grow:
  %twice = mul i64 %cap, 2
  %small = icmp ult i64 %twice, 64
  %cap1 = select i1 %small, i64 64, i64 %twice
  %bytes = mul i64 %cap1, 8
  %old = load ptr, ptr @conduit_dead
  %new = call ptr @realloc(ptr %old, i64 %bytes)
  %failed = icmp eq ptr %new, null
  br i1 %failed, label %oom, label %grown
oom:
  call void @llvm.trap()
  unreachable
grown:
  store ptr %new, ptr @conduit_dead
  store i64 %cap1, ptr @conduit_dead_cap
  br label %put
put:
  %s = load ptr, ptr @conduit_dead
  %e = getelementptr ptr, ptr %s, i64 %top
  store ptr %p, ptr %e
  %top1 = add i64 %top, 1
  store i64 %top1, ptr @conduit_dead_top
  ret void
}

; A step of %op on %c, which an object being looked at holds, and whose
; visit function is %v: 0 marks gray, taking one from its count; 1 scans;
; 2 marks black, giving it back; 3 collects white.
define tailcc void @conduit_step(ptr %c, ptr %v, i32 %op) {
entry:
  %null = icmp eq ptr %c, null
  br i1 %null, label %done, label %some
some:
  %h = load i32, ptr %c
  %immortal = icmp slt i32 %h, 0
  br i1 %immortal, label %done, label %go
go:
  switch i32 %op, label %push [ i32 0, label %gray i32 2, label %black ]
gray:
  %h1 = sub i32 %h, 1
  store i32 %h1, ptr %c
  br label %push
black:
  %h2 = add i32 %h, 1
  store i32 %h2, ptr %c
  %color = and i32 %h, 805306368
  %isblack = icmp eq i32 %color, 0
  br i1 %isblack, label %done, label %push
push:
  tail call tailcc void @conduit_push(ptr %c, ptr %v, i32 %op)
  ret void
done:
  ret void
}

; Does the steps on the stack until it's empty: marking gray, scanning,
; marking black and collecting white each object once, and visiting what
; it holds with the same step.
define tailcc void @conduit_run() {
entry:
  br label %loop
loop:
  %top = load i64, ptr @conduit_top
  %empty = icmp eq i64 %top, 0
  br i1 %empty, label %done, label %pop
pop:
  %t1 = sub i64 %top, 1
  store i64 %t1, ptr @conduit_top
  %s = load ptr, ptr @conduit_stack
  %e = getelementptr { ptr, ptr, i32 }, ptr %s, i64 %t1
  %p = load ptr, ptr %e
  %ve = getelementptr { ptr, ptr, i32 }, ptr %e, i64 0, i32 1
  %v = load ptr, ptr %ve
  %oe = getelementptr { ptr, ptr, i32 }, ptr %e, i64 0, i32 2
  %op = load i32, ptr %oe
  %h = load i32, ptr %p
  %color = and i32 %h, 805306368
  %base = and i32 %h, -805306369
  switch i32 %op, label %loop [ i32 0, label %gray i32 1, label %scan i32 2, label %black i32 3, label %white ]
gray:
  %isgray = icmp eq i32 %color, 268435456
  br i1 %isgray, label %loop, label %gray1
gray1:
  %hg = or i32 %base, 268435456
  store i32 %hg, ptr %p
  call tailcc void %v(ptr %p, i32 0)
  br label %loop
scan:
  %sgray = icmp eq i32 %color, 268435456
  br i1 %sgray, label %scan1, label %loop
scan1:
  %count = and i32 %h, 268435455
  %held = icmp ne i32 %count, 0
  br i1 %held, label %black1, label %white0
white0:
  %hw = or i32 %base, 536870912
  store i32 %hw, ptr %p
  call tailcc void %v(ptr %p, i32 1)
  br label %loop
black:
  %isblack = icmp eq i32 %color, 0
  br i1 %isblack, label %loop, label %black1
black1:
  store i32 %base, ptr %p
  call tailcc void %v(ptr %p, i32 2)
  br label %loop
white:
  %iswhite = icmp eq i32 %color, 536870912
  %bufbit = and i32 %h, 1073741824
  %unbuffered = icmp eq i32 %bufbit, 0
  %garbage = and i1 %iswhite, %unbuffered
  br i1 %garbage, label %white1, label %loop
white1:
  store i32 %base, ptr %p
  call tailcc void %v(ptr %p, i32 3)
  call tailcc void @conduit_bury(ptr %p)
  br label %loop
done:
  ret void
}

; Root %i of the buffer, and its visit function.
define tailcc { ptr, ptr } @conduit_root(i32 %i) {
  %i64 = zext i32 %i to i64
  %slot = getelementptr [256 x { ptr, ptr }], ptr @conduit_roots, i64 0, i64 %i64
  %r = load { ptr, ptr }, ptr %slot
  ret { ptr, ptr } %r
}

; Trial deletion of what's buffered: mark gray from each root that's still
; purple, and let the others go, freeing those whose count got to 0 while
; buffered; scan from each; then collect white from each, and free the
; garbage.
define tailcc void @conduit_collect_cycles() {
entry:
  %n = load i32, ptr @conduit_nroots
  br label %mark
mark:
  %i = phi i32 [ 0, %entry ], [ %i1, %mnext ]
  %j = phi i32 [ 0, %entry ], [ %j1, %mnext ]
  %more = icmp ult i32 %i, %n
  br i1 %more, label %mbody, label %marked
mbody:
  %r = call tailcc { ptr, ptr } @conduit_root(i32 %i)
  %p = extractvalue { ptr, ptr } %r, 0
  %v = extractvalue { ptr, ptr } %r, 1
  %h = load i32, ptr %p
  %unbuffered = and i32 %h, -1073741825
  %immortal = icmp slt i32 %h, 0
  br i1 %immortal, label %let_go, label %mortal
mortal:
  %color = and i32 %h, 805306368
  %count = and i32 %h, 268435455
  %purple = icmp eq i32 %color, 805306368
  %held = icmp ne i32 %count, 0
  %root = and i1 %purple, %held
  br i1 %root, label %keep, label %drop
keep:
  call tailcc void @conduit_push(ptr %p, ptr %v, i32 0)
  call tailcc void @conduit_run()
  %j64 = zext i32 %j to i64
  %jslot = getelementptr [256 x { ptr, ptr }], ptr @conduit_roots, i64 0, i64 %j64
  store { ptr, ptr } %r, ptr %jslot
  %jk = add i32 %j, 1
  br label %mnext
drop:
  store i32 %unbuffered, ptr %p
  %dead = icmp eq i32 %h, 1073741824
  br i1 %dead, label %free, label %mnext
free:
  call tailcc void @conduit_release(ptr %p)
  br label %mnext
let_go:
  store i32 %unbuffered, ptr %p
  br label %mnext
mnext:
  %j1 = phi i32 [ %jk, %keep ], [ %j, %drop ], [ %j, %free ], [ %j, %let_go ]
  %i1 = add i32 %i, 1
  br label %mark
marked:
  store i32 %j, ptr @conduit_nroots
  br label %scan
scan:
  %si = phi i32 [ 0, %marked ], [ %si1, %sbody ]
  %smore = icmp ult i32 %si, %j
  br i1 %smore, label %sbody, label %white
sbody:
  %sr = call tailcc { ptr, ptr } @conduit_root(i32 %si)
  %sp = extractvalue { ptr, ptr } %sr, 0
  %sv = extractvalue { ptr, ptr } %sr, 1
  call tailcc void @conduit_push(ptr %sp, ptr %sv, i32 1)
  call tailcc void @conduit_run()
  %si1 = add i32 %si, 1
  br label %scan
white:
  %wi = phi i32 [ 0, %scan ], [ %wi1, %wbody ]
  %wmore = icmp ult i32 %wi, %j
  br i1 %wmore, label %wbody, label %bury
wbody:
  %wr = call tailcc { ptr, ptr } @conduit_root(i32 %wi)
  %wp = extractvalue { ptr, ptr } %wr, 0
  %wv = extractvalue { ptr, ptr } %wr, 1
  %wh = load i32, ptr %wp
  %wu = and i32 %wh, -1073741825
  store i32 %wu, ptr %wp
  call tailcc void @conduit_push(ptr %wp, ptr %wv, i32 3)
  call tailcc void @conduit_run()
  %wi1 = add i32 %wi, 1
  br label %white
bury:
  store i32 0, ptr @conduit_nroots
  %dn = load i64, ptr @conduit_dead_top
  %ds = load ptr, ptr @conduit_dead
  br label %fl
fl:
  %fi = phi i64 [ 0, %bury ], [ %fi1, %fbody ]
  %fmore = icmp ult i64 %fi, %dn
  br i1 %fmore, label %fbody, label %done
fbody:
  %fe = getelementptr ptr, ptr %ds, i64 %fi
  %fp = load ptr, ptr %fe
  call tailcc void @conduit_release(ptr %fp)
  %fi1 = add i64 %fi, 1
  br label %fl
done:
  store i64 0, ptr @conduit_dead_top
  ret void
}

; Collects what's buffered now: for a host, or a test, to call at the end.
define void @conduit_collect() {
  call tailcc void @conduit_collect_cycles()
  ret void
}
"#;

/// The drop functions of `kinds` and of every kind they can hold, their
/// descriptors, and each data type's "children only", which `drop_reuse`
/// uses; with `cycles`, also the drops of `imm` objects and the visit
/// functions.
pub(crate) fn drops(m: &Module, kinds: &BTreeSet<Kind>, cycles: bool) -> Result<String, String> {
    let mut all: BTreeSet<Kind> = BTreeSet::new();
    let mut todo: Vec<Kind> = kinds.iter().cloned().collect();
    while let Some(k) = todo.pop() {
        if all.insert(k.clone()) {
            todo.extend(k.held(m));
        }
    }
    let mut out = String::new();
    for k in &all {
        out.push_str(&drop_fn(m, k, cycles, false)?);
        if cycles {
            out.push_str(&drop_fn(m, k, cycles, true)?);
            out.push_str(&visit_fn(m, k)?);
        }
        let visit = if cycles { k.visit_fn() } else { "null".into() };
        writeln!(
            out,
            "{} = private unnamed_addr constant {{ ptr, ptr }} {{ ptr {}, ptr {visit} }}",
            k.desc(),
            k.drop_fn()
        )
        .unwrap();
        if let Kind::Data(d) = k {
            out.push_str(&children_fn(m, *d)?);
        }
    }
    Ok(out)
}

/// What each constructor of `d` holds that's counted: its offset and kind.
fn data_fields(m: &Module, d: DataId) -> Result<Vec<Vec<(u64, Kind)>>, String> {
    let decl = m
        .data
        .get(&d)
        .ok_or_else(|| format!("unknown data type data{}", d.0))?;
    (0..decl.ctors.len())
        .map(|i| {
            let c = conduit_ir::CtorId {
                data: d,
                index: i as u32,
            };
            let tys = ctor_fields(m, c)?;
            let (offsets, _) = layout(&tys);
            Ok((tys.iter().zip(offsets))
                .filter_map(|(t, o)| Kind::of(t).map(|k| (o, k)))
                .collect())
        })
        .collect()
}

/// A switch on the constructor of `%p`, a block per constructor, each of
/// which loads what it holds into `%f<ctor>_<n>`, then does `each`.
fn per_ctor(
    m: &Module,
    d: DataId,
    out: &mut String,
    each: impl Fn(&mut String, &[(String, Kind)]),
) -> Result<(), String> {
    let ctors = data_fields(m, d)?;
    writeln!(out, "  %c = getelementptr i8, ptr %p, i64 4").unwrap();
    writeln!(out, "  %ctor = load i32, ptr %c").unwrap();
    let cases: Vec<String> = (0..ctors.len())
        .map(|i| format!("i32 {i}, label %k{i}"))
        .collect();
    writeln!(
        out,
        "  switch i32 %ctor, label %bad [ {} ]",
        cases.join(" ")
    )
    .unwrap();
    for (i, held) in ctors.iter().enumerate() {
        writeln!(out, "k{i}:").unwrap();
        let mut loaded = Vec::new();
        for (j, (o, k)) in held.iter().enumerate() {
            writeln!(out, "  %q{i}_{j} = getelementptr i8, ptr %p, i64 {o}").unwrap();
            writeln!(out, "  %f{i}_{j} = load ptr, ptr %q{i}_{j}").unwrap();
            loaded.push((format!("%f{i}_{j}"), k.clone()));
        }
        each(out, &loaded);
        writeln!(out, "  ret void").unwrap();
    }
    writeln!(out, "bad:\n  unreachable").unwrap();
    Ok(())
}

/// Calls the drop of each of `held`, tail-calling the last.
fn drop_all(out: &mut String, held: &[(String, Kind)], imm: bool) {
    for (j, (x, k)) in held.iter().enumerate() {
        let tail = if j + 1 == held.len() { "tail " } else { "" };
        let drop = if imm { k.drop_imm_fn() } else { k.drop_fn() };
        writeln!(out, "  {tail}call tailcc void {drop}(ptr {x})").unwrap();
    }
}

/// The drop of kind `k`, or with `imm`, its drop for `imm` objects, which
/// never buffers, and drops what it holds the same way.
fn drop_fn(m: &Module, k: &Kind, cycles: bool, imm: bool) -> Result<String, String> {
    let mut s = String::new();
    let name = if imm { k.drop_imm_fn() } else { k.drop_fn() };
    writeln!(s, "\ndefine tailcc void {name}(ptr %p) {{\nentry:").unwrap();
    if cycles && !imm {
        writeln!(
            s,
            "  %last = call tailcc i1 @conduit_last_buf(ptr %p, ptr {})",
            k.visit_fn()
        )
        .unwrap();
    } else {
        writeln!(s, "  %last = call tailcc i1 @conduit_last(ptr %p)").unwrap();
    }
    writeln!(
        s,
        "  br i1 %last, label %free, label %done\ndone:\n  ret void\nfree:"
    )
    .unwrap();
    let drop_of = |e: &Kind| if imm { e.drop_imm_fn() } else { e.drop_fn() };
    match k {
        Kind::Data(d) => per_ctor(m, *d, &mut s, |out, held| {
            writeln!(out, "  call tailcc void @conduit_free(ptr %p)").unwrap();
            drop_all(out, held, imm);
        })?,
        Kind::Array(e) => {
            if let Some(e) = e {
                // Dropping an element may collect cycles, which mustn't
                // see the array, or free it, while it's still read: with
                // a count of 0 and purple, it lets go of it if it's
                // buffered, for `conduit_free` to free.
                if cycles {
                    s.push_str(
                        "  %h0 = load i32, ptr %p\n  %b0 = and i32 %h0, 1073741824\n  \
                         %z0 = or i32 %b0, 805306368\n  store i32 %z0, ptr %p\n",
                    );
                }
                // Each element, then the memory.
                s.push_str(&format!(
                    "  %lq = getelementptr i8, ptr %p, i64 4\n  %n = load i32, ptr %lq\n  br label %head\n\
                     head:\n  %i = phi i32 [ 0, %free ], [ %i1, %body ]\n  %more = icmp slt i32 %i, %n\n  \
                     br i1 %more, label %body, label %end\n\
                     body:\n  %w = zext i32 %i to i64\n  %o = mul i64 %w, 8\n  %o8 = add i64 %o, 8\n  \
                     %q = getelementptr i8, ptr %p, i64 %o8\n  %x = load ptr, ptr %q\n  \
                     call tailcc void {}(ptr %x)\n  %i1 = add i32 %i, 1\n  br label %head\nend:\n",
                    drop_of(e)
                ));
            }
            s.push_str("  tail call tailcc void @conduit_free(ptr %p)\n  ret void\n");
        }
        Kind::Ref(e) => match e {
            Some(e) => s.push_str(&format!(
                "  %q = getelementptr i8, ptr %p, i64 8\n  %x = load ptr, ptr %q\n  \
                 call tailcc void @conduit_free(ptr %p)\n  tail call tailcc void {}(ptr %x)\n  ret void\n",
                drop_of(e)
            )),
            None => s.push_str("  tail call tailcc void @conduit_free(ptr %p)\n  ret void\n"),
        },
        // Its own function frees it and drops its captures.
        Kind::Closure => s.push_str(
            "  %q = getelementptr i8, ptr %p, i64 16\n  %d = load ptr, ptr %q\n  \
             %f = load ptr, ptr %d\n  tail call tailcc void %f(ptr %p)\n  ret void\n",
        ),
        // What it holds is dropped by its kind's drop, if it's counted.
        Kind::Any => s.push_str(
            "  %vq = getelementptr i8, ptr %p, i64 8\n  %v = load ptr, ptr %vq\n  \
             %dq = getelementptr i8, ptr %p, i64 16\n  %d = load ptr, ptr %dq\n  \
             call tailcc void @conduit_free(ptr %p)\n  %none = icmp eq ptr %d, null\n  \
             br i1 %none, label %end, label %held\nheld:\n  %f = load ptr, ptr %d\n  \
             tail call tailcc void %f(ptr %v)\n  ret void\nend:\n  ret void\n",
        ),
    }
    s.push_str("}\n");
    Ok(s)
}

/// The visit function of kind `k`: a step of the collector, `%op`, on
/// each object an object of kind `k` holds.
fn visit_fn(m: &Module, k: &Kind) -> Result<String, String> {
    let mut s = String::new();
    writeln!(
        s,
        "\ndefine tailcc void {}(ptr %p, i32 %op) {{\nentry:",
        k.visit_fn()
    )
    .unwrap();
    match k {
        Kind::Data(d) => per_ctor(m, *d, &mut s, step_all)?,
        Kind::Array(Some(e)) => s.push_str(&format!(
            "  %lq = getelementptr i8, ptr %p, i64 4\n  %n = load i32, ptr %lq\n  br label %head\n\
             head:\n  %i = phi i32 [ 0, %entry ], [ %i1, %body ]\n  %more = icmp slt i32 %i, %n\n  \
             br i1 %more, label %body, label %end\n\
             body:\n  %w = zext i32 %i to i64\n  %o = mul i64 %w, 8\n  %o8 = add i64 %o, 8\n  \
             %q = getelementptr i8, ptr %p, i64 %o8\n  %x = load ptr, ptr %q\n  \
             call tailcc void @conduit_step(ptr %x, ptr {}, i32 %op)\n  %i1 = add i32 %i, 1\n  \
             br label %head\nend:\n  ret void\n",
            e.visit_fn()
        )),
        Kind::Ref(Some(e)) => s.push_str(&format!(
            "  %q = getelementptr i8, ptr %p, i64 8\n  %x = load ptr, ptr %q\n  \
             tail call tailcc void @conduit_step(ptr %x, ptr {}, i32 %op)\n  ret void\n",
            e.visit_fn()
        )),
        Kind::Array(None) | Kind::Ref(None) => s.push_str("  ret void\n"),
        // Its own visit function, in its descriptor, if it has one.
        Kind::Closure => s.push_str(
            "  %q = getelementptr i8, ptr %p, i64 16\n  %d = load ptr, ptr %q\n  \
             %none = icmp eq ptr %d, null\n  br i1 %none, label %end, label %some\n\
             some:\n  %vq = getelementptr i8, ptr %d, i64 8\n  %v = load ptr, ptr %vq\n  \
             tail call tailcc void %v(ptr %p, i32 %op)\n  ret void\nend:\n  ret void\n",
        ),
        Kind::Any => s.push_str(
            "  %dq = getelementptr i8, ptr %p, i64 16\n  %d = load ptr, ptr %dq\n  \
             %none = icmp eq ptr %d, null\n  br i1 %none, label %end, label %some\n\
             some:\n  %xq = getelementptr i8, ptr %p, i64 8\n  %x = load ptr, ptr %xq\n  \
             %vq = getelementptr i8, ptr %d, i64 8\n  %v = load ptr, ptr %vq\n  \
             tail call tailcc void @conduit_step(ptr %x, ptr %v, i32 %op)\n  ret void\n\
             end:\n  ret void\n",
        ),
    }
    s.push_str("}\n");
    Ok(s)
}

/// Calls `@conduit_step` on each of `held`, with `%op`.
fn step_all(out: &mut String, held: &[(String, Kind)]) {
    for (x, k) in held {
        writeln!(
            out,
            "  call tailcc void @conduit_step(ptr {x}, ptr {}, i32 %op)",
            k.visit_fn()
        )
        .unwrap();
    }
}

/// `@conduit_children.data<d>(p)`: drops what a data object holds, but
/// keeps its memory, for `drop_reuse`.
fn children_fn(m: &Module, d: DataId) -> Result<String, String> {
    let mut s = String::new();
    writeln!(
        s,
        "\ndefine tailcc void @conduit_children.data{}(ptr %p) {{\nentry:",
        d.0
    )
    .unwrap();
    per_ctor(m, d, &mut s, |out, held| drop_all(out, held, false))?;
    s.push_str("}\n");
    Ok(s)
}

/// The counted captures of a closure, at these offsets, and their kinds.
fn captured(captures: &[Ty], offsets: &[u64]) -> Vec<(u64, Kind)> {
    (captures.iter().zip(offsets))
        .filter_map(|(t, &o)| Kind::of(t).map(|k| (o, k)))
        .collect()
}

/// The function that frees a closure with these captures, at these
/// offsets, and drops them.
pub(crate) fn closure_free(name: &str, captures: &[Ty], offsets: &[u64]) -> String {
    let mut s = String::new();
    writeln!(s, "\ndefine tailcc void {name}(ptr %c) {{").unwrap();
    let held = captured(captures, offsets);
    for (j, (o, _)) in held.iter().enumerate() {
        writeln!(s, "  %q{j} = getelementptr i8, ptr %c, i64 {o}").unwrap();
        writeln!(s, "  %x{j} = load ptr, ptr %q{j}").unwrap();
    }
    writeln!(s, "  call tailcc void @conduit_free(ptr %c)").unwrap();
    for (j, (_, k)) in held.iter().enumerate() {
        let tail = if j + 1 == held.len() { "tail " } else { "" };
        writeln!(s, "  {tail}call tailcc void {}(ptr %x{j})", k.drop_fn()).unwrap();
    }
    s.push_str("  ret void\n}\n");
    s
}

/// The visit function of a closure with these captures, at these offsets.
pub(crate) fn closure_visit(name: &str, captures: &[Ty], offsets: &[u64]) -> String {
    let mut s = String::new();
    writeln!(s, "\ndefine tailcc void {name}(ptr %p, i32 %op) {{").unwrap();
    let held = captured(captures, offsets);
    let mut loaded = Vec::new();
    for (j, (o, k)) in held.iter().enumerate() {
        writeln!(s, "  %q{j} = getelementptr i8, ptr %p, i64 {o}").unwrap();
        writeln!(s, "  %x{j} = load ptr, ptr %q{j}").unwrap();
        loaded.push((format!("%x{j}"), k.clone()));
    }
    step_all(&mut s, &loaded);
    s.push_str("  ret void\n}\n");
    s
}
