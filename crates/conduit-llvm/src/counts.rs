//! Reference counting, for a module whose counts are explicit, as
//! `conduit_opt::ownership::insert_counts` makes them, when
//! [`Options::counts`](crate::Options::counts) asks for it.
//!
//! - **The header word** at 0 is the count. A negative one is immortal,
//!   as a constant's is, and counting skips it. A count that would pass
//!   `0x0fff_ffff` saturates instead: the object turns immortal and leaks,
//!   rather than being freed while it's still in use, as in the Wasm
//!   backend and the evaluator.
//! - **Dropping** goes through a function for each kind of object, a
//!   [`Kind`]: a data type, arrays and refs of what they hold, closures and
//!   `any`. At 0, it frees the object and drops what it held, tail-calling
//!   the drop of its last field, so that dropping a list is a loop. A
//!   closure holds, at 16, the function that frees it and drops its
//!   captures, and an `any`, at 16, the drop of what it holds, or null.
//! - **`@conduit_live`** counts the objects allocated and not yet freed, for
//!   tests to compare with the evaluator's.
//!
//! Cycles aren't collected yet: what's left in one leaks.

use std::collections::BTreeSet;
use std::fmt::Write;

use conduit_ir::{DataId, Module, Ty};

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

    /// The function that drops an object of this kind.
    pub fn drop_fn(&self) -> String {
        format!("@conduit_drop.{}", self.name())
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

/// The runtime every counted module has: `dup`, `add`, `decref`, `release`
/// and the counter of live objects.
pub(crate) fn runtime() -> &'static str {
    r#"
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
  %h64 = zext i32 %h to i64
  %k64 = zext i32 %k to i64
  %sum = add i64 %h64, %k64
  %over = icmp ugt i64 %sum, 268435455
  %s32 = trunc i64 %sum to i32
  %new = select i1 %over, i32 -2147483648, i32 %s32
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
  %one = icmp eq i32 %h, 1
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
  %one = icmp eq i32 %h, 1
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
"#
}

/// The drop functions of `kinds` and of every kind they can hold, and of
/// each object's "children only" for data, which `drop_reuse` uses.
pub(crate) fn drops(m: &Module, kinds: &BTreeSet<Kind>) -> Result<String, String> {
    let mut all: BTreeSet<Kind> = BTreeSet::new();
    let mut todo: Vec<Kind> = kinds.iter().cloned().collect();
    while let Some(k) = todo.pop() {
        if all.insert(k.clone()) {
            todo.extend(k.held(m));
        }
    }
    let mut out = String::new();
    for k in &all {
        out.push_str(&drop_fn(m, k)?);
        if let Kind::Data(d) = k {
            out.push_str(&children_fn(m, *d)?);
        }
    }
    Ok(out)
}

/// What a data object of `d` holds, as loads into `%f<i>`, and the drops
/// that follow, by constructor.
fn data_fields(m: &Module, d: DataId) -> Result<Vec<Vec<(String, String)>>, String> {
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
                .filter_map(|(t, o)| Kind::of(t).map(|k| (o.to_string(), k.drop_fn())))
                .collect())
        })
        .collect()
}

/// A switch on the constructor of `%p`, a block per constructor, each of
/// which loads what it holds, runs `release` if given, and drops it.
fn per_ctor(m: &Module, d: DataId, release: bool, out: &mut String) -> Result<(), String> {
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
        for (j, (o, _)) in held.iter().enumerate() {
            writeln!(out, "  %q{i}_{j} = getelementptr i8, ptr %p, i64 {o}").unwrap();
            writeln!(out, "  %f{i}_{j} = load ptr, ptr %q{i}_{j}").unwrap();
        }
        if release {
            writeln!(out, "  call tailcc void @conduit_release(ptr %p)").unwrap();
        }
        for (j, (_, drop)) in held.iter().enumerate() {
            let tail = if j + 1 == held.len() { "tail " } else { "" };
            writeln!(out, "  {tail}call tailcc void {drop}(ptr %f{i}_{j})").unwrap();
        }
        writeln!(out, "  ret void").unwrap();
    }
    writeln!(out, "bad:\n  unreachable").unwrap();
    Ok(())
}

fn drop_fn(m: &Module, k: &Kind) -> Result<String, String> {
    let mut s = String::new();
    writeln!(s, "\ndefine tailcc void {}(ptr %p) {{\nentry:", k.drop_fn()).unwrap();
    writeln!(s, "  %last = call tailcc i1 @conduit_last(ptr %p)").unwrap();
    writeln!(
        s,
        "  br i1 %last, label %free, label %done\ndone:\n  ret void\nfree:"
    )
    .unwrap();
    match k {
        Kind::Data(d) => per_ctor(m, *d, true, &mut s)?,
        Kind::Array(e) => {
            if let Some(e) = e {
                // Each element, then the memory.
                s.push_str(&format!(
                    "  %lq = getelementptr i8, ptr %p, i64 4\n  %n = load i32, ptr %lq\n  br label %head\n\
                     head:\n  %i = phi i32 [ 0, %free ], [ %i1, %body ]\n  %more = icmp slt i32 %i, %n\n  \
                     br i1 %more, label %body, label %end\n\
                     body:\n  %w = zext i32 %i to i64\n  %o = mul i64 %w, 8\n  %o8 = add i64 %o, 8\n  \
                     %q = getelementptr i8, ptr %p, i64 %o8\n  %x = load ptr, ptr %q\n  \
                     call tailcc void {}(ptr %x)\n  %i1 = add i32 %i, 1\n  br label %head\nend:\n",
                    e.drop_fn()
                ));
            }
            s.push_str("  tail call tailcc void @conduit_release(ptr %p)\n  ret void\n");
        }
        Kind::Ref(e) => match e {
            Some(e) => s.push_str(&format!(
                "  %q = getelementptr i8, ptr %p, i64 8\n  %x = load ptr, ptr %q\n  \
                 call tailcc void @conduit_release(ptr %p)\n  tail call tailcc void {}(ptr %x)\n  ret void\n",
                e.drop_fn()
            )),
            None => s.push_str("  tail call tailcc void @conduit_release(ptr %p)\n  ret void\n"),
        },
        // Its own function frees it and drops its captures.
        Kind::Closure => s.push_str(
            "  %q = getelementptr i8, ptr %p, i64 16\n  %f = load ptr, ptr %q\n  \
             tail call tailcc void %f(ptr %p)\n  ret void\n",
        ),
        // What it holds is dropped by the function at 16, if there's one.
        Kind::Any => s.push_str(
            "  %vq = getelementptr i8, ptr %p, i64 8\n  %v = load ptr, ptr %vq\n  \
             %dq = getelementptr i8, ptr %p, i64 16\n  %d = load ptr, ptr %dq\n  \
             call tailcc void @conduit_release(ptr %p)\n  %none = icmp eq ptr %d, null\n  \
             br i1 %none, label %end, label %held\nheld:\n  tail call tailcc void %d(ptr %v)\n  ret void\n\
             end:\n  ret void\n",
        ),
    }
    s.push_str("}\n");
    Ok(s)
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
    per_ctor(m, d, false, &mut s)?;
    s.push_str("}\n");
    Ok(s)
}

/// The function that frees a closure of `f` with these captures, at
/// these offsets, and drops them.
pub(crate) fn closure_free(name: &str, captures: &[Ty], offsets: &[u64]) -> Result<String, String> {
    let mut s = String::new();
    writeln!(s, "\ndefine tailcc void {name}(ptr %c) {{").unwrap();
    let held: Vec<(u64, Kind)> = (captures.iter().zip(offsets))
        .filter_map(|(t, &o)| Kind::of(t).map(|k| (o, k)))
        .collect();
    for (j, (o, _)) in held.iter().enumerate() {
        writeln!(s, "  %q{j} = getelementptr i8, ptr %c, i64 {o}").unwrap();
        writeln!(s, "  %x{j} = load ptr, ptr %q{j}").unwrap();
    }
    writeln!(s, "  call tailcc void @conduit_release(ptr %c)").unwrap();
    for (j, (_, k)) in held.iter().enumerate() {
        let tail = if j + 1 == held.len() { "tail " } else { "" };
        writeln!(s, "  {tail}call tailcc void {}(ptr %x{j})", k.drop_fn()).unwrap();
    }
    s.push_str("  ret void\n}\n");
    Ok(s)
}
