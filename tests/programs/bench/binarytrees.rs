//! binarytrees.conduit by hand, in Rust, for conduit-bench's `rust`
//! backend: the same trees, each built whole in `Box`es, checked, and
//! dropped. Integers wrap, as in Conduit.

enum Tree {
    Leaf(i32),
    Node(Box<Tree>, i32, Box<Tree>),
}

fn make(item: i32, d: i32) -> Box<Tree> {
    Box::new(if d == 0 {
        Tree::Leaf(item)
    } else {
        let i2 = item.wrapping_mul(2);
        Tree::Node(make(i2.wrapping_sub(1), d - 1), item, make(i2, d - 1))
    })
}

fn check(t: &Tree) -> i32 {
    match t {
        Tree::Leaf(x) => *x,
        Tree::Node(l, x, r) => x.wrapping_add(check(l)).wrapping_sub(check(r)),
    }
}

fn trees(d: i32, n: i32) -> i32 {
    let mut acc = 0i32;
    for i in (1..=n).rev() {
        let t = make(i, d);
        acc = acc.wrapping_add(check(&t));
    }
    acc
}

fn run(k: i32) -> i32 {
    match k {
        0 => trees(6, 20),
        1 => trees(16, 20),
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
