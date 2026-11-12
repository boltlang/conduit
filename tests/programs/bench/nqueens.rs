//! nqueens.conduit by hand, in Rust, for conduit-bench's `rust` backend:
//! the same search, with the queens placed so far in a list whose cells
//! are borrowed from the stack, since none outlives the call that makes it.

struct List<'a> {
    head: i32,
    tail: Option<&'a List<'a>>,
}

fn safe(q: i32, mut d: i32, mut qs: Option<&List<'_>>) -> bool {
    while let Some(cell) = qs {
        let h = cell.head;
        if q == h || q == h + d || q == h - d {
            return false;
        }
        qs = cell.tail;
        d += 1;
    }
    true
}

fn place(n: i32, row: i32, qs: Option<&List<'_>>) -> i32 {
    if row == n {
        return 1;
    }
    let mut acc = 0;
    for c in 0..n {
        if safe(c, 1, qs) {
            let cell = List { head: c, tail: qs };
            acc += place(n, row + 1, Some(&cell));
        }
    }
    acc
}

fn run(k: i32) -> i32 {
    match k {
        0 => place(6, 0, None),
        1 => place(8, 0, None),
        _ => std::process::abort(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn conduit_print_run(k: i32) {
    use std::io::Write;
    print!("{:x} ", run(k) as u32);
    std::io::stdout().flush().unwrap();
}

#[unsafe(no_mangle)]
pub extern "C" fn conduit_bench_run(k: i32) -> i64 {
    run(k) as u32 as i64
}
