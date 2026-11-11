//! Exports that take and give host values, and functions `host_function`
//! makes, called from C: a host value
//! given to Conduit is Conduit's reference, one given back is retained for
//! the caller, and every box is released and freed once nothing holds it.
//! Results after the first, of externs and exports, go through pointers.
//! Skipped, with a note, when `clang` isn't installed.

use std::path::PathBuf;
use std::process::Command;

const MODULE: &str = r#"
#[export(c, symbol = "echo")]
fn echo(%h: host) -> (host) {
  jump ret(%h)
}

#[export(c, symbol = "twice")]
fn twice(%h: host) -> (host, i32, host) {
  jump ret(%h, 7i32, %h)
}

#[link(c, symbol = "split")]
extern split(i32) -> (i32, host, i64)

#[export(c, symbol = "use_split")]
fn use_split(%n: i32) -> (i32, host, i64) {
  let %a: i32, %h: host, %b: i64 = call_extern split(%n)
  jump ret(%a, %h, %b)
}

#[export(c, symbol = "forget_it")]
fn forget_it(%h: host) -> (i32) {
  jump ret(1i32)
}

#[export(c, symbol = "or_null")]
fn or_null(%h: nullable host, %keep: i32) -> (nullable host) {
  case %keep {
    0 =>
      let %n: nullable host = host_null()
      jump ret(%n)
    _ => jump ret(%h)
  }
}
"#;

const MAIN: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct { void (*retain)(void *); void (*release)(void *); } ops_t;
typedef struct { void *value; const ops_t *ops; } host;

extern int64_t conduit_live;
host echo(void *value, const ops_t *ops);
host twice(void *value, const ops_t *ops, int32_t *n, host *b);
int32_t use_split(int32_t n, host *h, int64_t *big);
int32_t forget_it(void *value, const ops_t *ops);
host or_null(void *value, const ops_t *ops, int32_t keep);

static int refs = 1;
static void retain(void *v) { refs++; }
static void release(void *v) { refs--; }
static const ops_t ops = {retain, release};
static int value;

// Gives Conduit a reference of its own.
int32_t split(int32_t n, host *h, int64_t *big) {
  retain(&value);
  h->value = &value;
  h->ops = &ops;
  *big = (int64_t)n * 1000000000000;
  return n + 1;
}

#define CHECK(c) do { if (!(c)) { printf("line %d: %s, refs %d, live %lld\n", __LINE__, #c, refs, (long long)conduit_live); return 1; } } while (0)

int main(void) {
  // Each call is given a reference of its own.
  retain(&value);
  host h = echo(&value, &ops);
  CHECK(h.value == &value && h.ops == &ops && refs == 2);
  release(&value);

  retain(&value);
  int32_t n7;
  host b;
  host a = twice(&value, &ops, &n7, &b);
  CHECK(a.value == &value && b.value == &value && b.ops == &ops && n7 == 7 && refs == 3);
  release(&value);
  release(&value);

  host s;
  int64_t big;
  CHECK(use_split(5, &s, &big) == 6 && s.value == &value && big == 5000000000000 && refs == 2);
  release(&value);

  retain(&value);
  CHECK(forget_it(&value, &ops) == 1 && refs == 1);

  retain(&value);
  host n = or_null(&value, &ops, 0);
  CHECK(n.value == NULL && refs == 1);
  host k = or_null(NULL, NULL, 1);
  CHECK(k.value == NULL && refs == 1);

  CHECK(conduit_live == 0);
  return 0;
}
"#;

/// A module whose functions take and give closures the host calls, made
/// by `host_function`. The host makes a function of a trampoline and a
/// closure, which it counts the references to itself, and lets go of the
/// closure with `conduit_release` when its count gets to 0.
const CALLBACKS: &str = r#"
#[link(c, symbol = "keep")]
extern keep(host) -> ()

fn pair(%k: i32, %x: i32) -> (i32, i64) {
  let %s: i32 = add(%x, %k)
  let %p: i32 = mul(%x, %k)
  let %w: i64 = convert.s[i64](%p)
  jump ret(%s, %w)
}

#[export(c, symbol = "setup")]
fn setup(%k: i32) -> (i32) {
  let %c: mut fn(i32) -> (i32, i64) = closure pair(%k)
  let %h: host = host_function(%c)
  let = call_extern keep(%h)
  jump ret(1i32)
}
"#;

const CALLBACKS_MAIN: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct { void (*retain)(void *); void (*release)(void *); } ops_t;
typedef struct { void *value; const ops_t *ops; } host;

extern int64_t conduit_live;
void conduit_release(void *closure);
int32_t setup(int32_t k);

// A function made of a trampoline and a closure, which the host counts.
typedef int32_t (*tramp_t)(void *closure, int32_t x, int64_t *product);
typedef struct { tramp_t tramp; void *closure; int refs; } callback;
static int live_callbacks = 0;

static void cb_retain(void *v) { ((callback *)v)->refs++; }
static void cb_release(void *v) {
  callback *c = v;
  if (--c->refs == 0) {
    conduit_release(c->closure);
    free(c);
    live_callbacks--;
  }
}
static const ops_t cb_ops = {cb_retain, cb_release};

host conduit_host_function(void *tramp, void *closure) {
  callback *c = malloc(sizeof *c);
  c->tramp = tramp;
  c->closure = closure;
  c->refs = 1;
  live_callbacks++;
  return (host){c, &cb_ops};
}

// It borrows what it's given, so it keeps a reference of its own.
static callback *registered;
void keep(void *value) {
  cb_retain(value);
  registered = value;
}

#define CHECK(c) do { if (!(c)) { printf("line %d: %s, live %lld\n", __LINE__, #c, (long long)conduit_live); return 1; } } while (0)

int main(void) {
  CHECK(setup(10) == 1);
  // Conduit let go of its reference, and the host keeps its own.
  CHECK(registered->refs == 1 && live_callbacks == 1);
  int64_t product;
  CHECK(registered->tramp(registered->closure, 5, &product) == 15 && product == 50);
  CHECK(registered->tramp(registered->closure, 3, &product) == 13 && product == 30);
  cb_release(registered);
  CHECK(live_callbacks == 0 && conduit_live == 0);
  return 0;
}
"#;

#[test]
fn host_exports() {
    run("host_exports", MODULE, MAIN);
}

#[test]
fn host_functions() {
    run("host_functions", CALLBACKS, CALLBACKS_MAIN);
}

/// Compiles `module`, with counts, and `main`, a C program, together, and
/// runs it, which must succeed. Does nothing but say so if `clang` isn't
/// installed.
fn run(name: &str, module: &str, main: &str) {
    let clang = Command::new("clang").arg("--version").output();
    if !clang.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped, since clang isn't installed");
        return;
    }
    let mut m = conduit_ir::text::parse(module).unwrap();
    conduit_ir::verify::verify_module(&m).unwrap();
    conduit_opt::ownership::insert_counts(&mut m).unwrap();
    let options = conduit_llvm::Options {
        counts: true,
        cycles: false,
    };
    let ll = conduit_llvm::emit_module_with(&m, &options).unwrap();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("llvm");
    std::fs::create_dir_all(&dir).unwrap();
    let (ll_path, c_path, exe) = (
        dir.join(format!("{name}.ll")),
        dir.join(format!("{name}.c")),
        dir.join(name),
    );
    std::fs::write(&ll_path, ll).unwrap();
    std::fs::write(&c_path, main).unwrap();
    let out = Command::new("clang")
        .args(["-O2", "-Wno-override-module", "-o"])
        .args([&exe, &ll_path, &c_path])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    assert!(
        run.status.success(),
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}
