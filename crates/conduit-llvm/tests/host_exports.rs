//! Exports that take and give host values, called from C: a host value
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

#[test]
fn host_exports() {
    let clang = Command::new("clang").arg("--version").output();
    if !clang.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped, since clang isn't installed");
        return;
    }
    let mut m = conduit_ir::text::parse(MODULE).unwrap();
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
        dir.join("host_exports.ll"),
        dir.join("host_exports.c"),
        dir.join("host_exports"),
    );
    std::fs::write(&ll_path, ll).unwrap();
    std::fs::write(&c_path, MAIN).unwrap();
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
