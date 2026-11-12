//! binarytrees.conduit by hand, in Rust, for conduit-bench's `rust`
//! backend: each tree is built whole, in `Box`es, counted, and dropped,
//! which is the work the benchmark is about. (Conduit's optimizer shares
//! the two equal subtrees of `make`, since it's a pure function of its
//! depth, so it builds far fewer nodes than this does.)

enum Tree {
    Leaf,
    Node(Box<Tree>, Box<Tree>),
}

fn make(d: i32) -> Box<Tree> {
    Box::new(if d == 0 {
        Tree::Leaf
    } else {
        Tree::Node(make(d - 1), make(d - 1))
    })
}

fn check(t: &Tree) -> i32 {
    match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => check(l).wrapping_add(check(r)).wrapping_add(1),
    }
}

fn trees(d: i32, n: i32) -> i32 {
    let mut acc = 0i32;
    for _ in 0..n {
        let t = make(d);
        acc = acc.wrapping_add(check(&t));
    }
    acc
}

fn run(k: i32) -> i32 {
    match k {
        0 => trees(6, 20),
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
