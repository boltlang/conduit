//! bstinsert.conduit by hand, in Rust, for conduit-bench's `rust` backend:
//! the same keys inserted into an unbalanced search tree, which Rust
//! changes in place, as reuse makes Conduit's copy of each path, and drops
//! at the end.

struct Node {
    left: Tree,
    key: i32,
    right: Tree,
}

type Tree = Option<Box<Node>>;

fn insert(mut at: &mut Tree, k: i32) {
    while let Some(t) = at {
        if k < t.key {
            at = &mut t.left;
        } else if k > t.key {
            at = &mut t.right;
        } else {
            return;
        }
    }
    *at = Some(Box::new(Node {
        left: None,
        key: k,
        right: None,
    }));
}

fn sum(t: &Tree) -> i32 {
    match t {
        None => 0,
        Some(t) => sum(&t.left).wrapping_add(sum(&t.right)).wrapping_add(t.key),
    }
}

fn bst(n: i32) -> i32 {
    let mut t = None;
    for i in 0..n {
        insert(&mut t, i.wrapping_mul(37) % 101);
    }
    sum(&t)
}

fn bst_random(n: i32) -> i32 {
    let mut t = None;
    let mut x = 1i32;
    for _ in 0..n {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        insert(&mut t, x & 0x7fffffff);
    }
    sum(&t)
}

fn run(k: i32) -> i32 {
    match k {
        0 => bst(200),
        1 => bst_random(100000),
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
