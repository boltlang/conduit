//! Debug info from where the frontend says the IR's sources are (#50):
//! each function with sources is a subprogram named after its debug name,
//! and its code has the lines of the `let`s and `jump`s it comes from, as
//! `llvm-dwarfdump` reads them from what `clang` makes. Skipped, with a
//! note, when `clang` or `llvm-dwarfdump` isn't installed.

use std::path::PathBuf;
use std::process::Command;

use conduit_ir::SourceId;
use conduit_llvm::{Location, Options, Sources};

const MODULE: &str = r#"
data Cell {
  (i32)
}

fn square(%x: i32) -> (i32) {
  let %y: i32 = mul(%x, %x) @src3
  jump ret(%y) @src4
}

#[export(c, symbol = "sum_squares")]
fn sum_squares(%n: i32) -> (i32) {
  let %c: mut Cell = new Cell.0(%n) @src8
  let %a: i32 = call square(%n) @src9
  let %b: i32 = get Cell.0.0(%c) @src10
  let %s: i32 = add(%a, %b) @src11
  jump ret(%s) @src12
}
"#;

fn installed(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// What `llvm-dwarfdump` with `flag` says of the object `clang` makes of
/// `ll`.
fn dwarf(name: &str, ll: &str, flag: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("llvm");
    std::fs::create_dir_all(&dir).unwrap();
    let (ll_path, obj) = (
        dir.join(format!("{name}.ll")),
        dir.join(format!("{name}.o")),
    );
    std::fs::write(&ll_path, ll).unwrap();
    let out = Command::new("clang")
        .args(["-c", "-O0", "-Wno-override-module", "-o"])
        .args([&obj, &ll_path])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("llvm-dwarfdump")
        .arg(flag)
        .arg(&obj)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn functions_and_lines_come_from_the_sources() {
    if !installed("clang") || !installed("llvm-dwarfdump") {
        eprintln!("skipped, since clang or llvm-dwarfdump isn't installed");
        return;
    }
    let mut m = conduit_ir::text::parse(MODULE).unwrap();
    conduit_ir::verify::verify_module(&m).unwrap();
    conduit_opt::ownership::insert_counts(&mut m).unwrap();
    // Each `@srcN` at line N of `squares.bolt`, column 5.
    let sources = Sources {
        directory: "/home/someone/project".into(),
        locations: [3, 4, 8, 9, 10, 11, 12]
            .into_iter()
            .map(|n| {
                let at = Location {
                    file: "squares.bolt".into(),
                    line: n,
                    column: 5,
                };
                (SourceId(n.into()), at)
            })
            .collect(),
    };
    let options = Options {
        counts: true,
        cycles: false,
    };
    let ll = conduit_llvm::emit_module_with_sources(&m, &options, &sources).unwrap();

    let info = dwarf("debug_info", &ll, "--debug-info");
    for name in ["\"square\"", "\"sum_squares\"", "\"squares.bolt\""] {
        assert!(info.contains(name), "no {name} in:\n{info}");
    }
    let lines = dwarf("debug_info", &ll, "--debug-line");
    for line in [3, 4, 8, 9, 10, 11, 12] {
        let row = format!(" {line:>6}      5 ");
        assert!(
            lines.lines().any(|l| l.contains(&row)),
            "no row for line {line}, column 5 in:\n{lines}"
        );
    }

    // Without sources, no debug info at all.
    let plain = conduit_llvm::emit_module_with(&m, &options).unwrap();
    assert!(!plain.contains("!dbg") && !plain.contains("DICompileUnit"));
}
